use std::sync::Arc;

#[cfg(feature = "utoipa")]
use axum::Json;
#[cfg(any(feature = "embed-ui", feature = "embed-docs"))]
use axum::response::Response;
#[cfg(any(feature = "server", feature = "sso", feature = "saml"))]
use axum::routing::post;
#[cfg(feature = "server")]
use axum::{Router, routing::get};
#[cfg(any(feature = "embed-ui", feature = "embed-docs"))]
use axum::{body::Body, response::IntoResponse};
#[cfg(any(feature = "embed-ui", feature = "embed-docs"))]
use http::StatusCode;
#[cfg(any(feature = "server", feature = "embed-ui", feature = "embed-docs"))]
use http::header;
use reqwest::Client;
#[cfg(any(feature = "embed-ui", feature = "embed-docs"))]
use rust_embed::Embed;
#[cfg(feature = "server")]
use tokio_util::task::TaskTracker;
#[cfg(feature = "server")]
use tower_http::services::{ServeDir, ServeFile};
#[cfg(feature = "server")]
use tower_http::set_header::SetResponseHeaderLayer;
#[cfg(feature = "server")]
use tower_http::{limit::RequestBodyLimitLayer, trace::TraceLayer};
#[cfg(feature = "utoipa")]
use utoipa_scalar::{Scalar, Servable};

#[cfg(feature = "prometheus")]
use crate::observability;
#[cfg(feature = "utoipa")]
use crate::openapi;
#[cfg(feature = "server")]
use crate::runtimes;
#[cfg(feature = "server")]
use crate::streaming;
use crate::{
    auth, authz, cache, catalog, config, db, dlq, events, guardrails,
    init::create_provider_instance, jobs, models, pricing, providers, secrets, services,
    usage_buffer,
};
#[cfg(feature = "server")]
use crate::{middleware, routes};

/// Embedded UI assets from ui/dist directory.
/// These are compiled into the binary at build time.
#[cfg(feature = "embed-ui")]
#[derive(Embed)]
#[folder = "ui/dist"]
// Source maps are built (vite `sourcemap: true`) but have no business in the
// shipped binary — excluding them drops tens of MB of embedded weight.
#[exclude = "**/*.map"]
#[allow_missing = true]
struct UiAssets;

/// Embedded documentation site assets from docs/out directory.
/// These are compiled into the binary at build time.
#[cfg(feature = "embed-docs")]
#[derive(Embed)]
#[folder = "docs/out"]
// Exclude source maps from the embedded docs site (see UiAssets).
#[exclude = "**/*.map"]
#[allow_missing = true]
struct DocsAssets;

/// Handler for serving embedded UI assets
#[cfg(feature = "embed-ui")]
async fn serve_embedded_asset(
    axum::extract::Path(path): axum::extract::Path<String>,
) -> impl IntoResponse {
    serve_embedded_file(&path)
}

/// Handler for serving embedded UI index at root
#[cfg(feature = "embed-ui")]
async fn serve_embedded_index() -> Response {
    serve_embedded_file("index.html")
}

#[cfg(feature = "embed-ui")]
fn serve_embedded_file(path: &str) -> Response {
    // Try to get the file, or fall back to index.html for SPA routing
    let file = UiAssets::get(path).or_else(|| UiAssets::get("index.html"));

    match file {
        Some(content) => {
            // rust-embed with mime-guess feature provides mimetype in metadata
            let content_type = content.metadata.mimetype();

            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, content_type)
                .body(Body::from(content.data.into_owned()))
                .unwrap()
        }
        None => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::from("Not Found"))
            .unwrap(),
    }
}

/// Add routes for serving static UI files
#[cfg(feature = "server")]
fn add_ui_routes(app: Router<AppState>, config: &config::GatewayConfig) -> Router<AppState> {
    use config::AssetSource;

    let ui_path = config.ui.path.trim_end_matches('/');

    match &config.ui.assets.source {
        #[cfg(feature = "server")]
        AssetSource::Filesystem { path } => {
            let assets_path = std::path::Path::new(path);
            let index_file = assets_path.join("index.html");

            if !assets_path.exists() {
                tracing::warn!(path = %path, "UI assets directory does not exist");
                return app;
            }

            tracing::info!(path = %path, ui_path = %ui_path, "Serving UI from filesystem");

            // ServeDir with fallback to index.html for SPA routing
            let serve_dir = ServeDir::new(path).fallback(ServeFile::new(&index_file));

            // Add cache-control header for assets
            let cache_control = config.ui.assets.cache_control.clone();
            let serve_dir_with_headers = tower::ServiceBuilder::new()
                .layer(SetResponseHeaderLayer::if_not_present(
                    header::CACHE_CONTROL,
                    header::HeaderValue::from_str(&cache_control).unwrap_or_else(|_| {
                        header::HeaderValue::from_static("public, max-age=3600")
                    }),
                ))
                .service(serve_dir);

            if ui_path.is_empty() || ui_path == "/" {
                // Serve at root - use fallback_service so other routes take precedence
                app.fallback_service(serve_dir_with_headers)
            } else {
                // Serve at a specific path
                app.nest_service(ui_path, serve_dir_with_headers)
            }
        }
        #[cfg(not(feature = "server"))]
        AssetSource::Filesystem { .. } => {
            tracing::warn!(
                "Filesystem UI assets requested but 'server' feature is not enabled, skipping"
            );
            app
        }
        #[cfg(feature = "embed-ui")]
        AssetSource::Embedded => {
            tracing::info!(ui_path = %ui_path, "Serving UI from embedded assets");

            // Create routes for embedded assets (stateless, so use Router<()>)
            let embedded_routes: Router<()> = Router::new()
                .route("/", get(serve_embedded_index))
                .route("/{*path}", get(serve_embedded_asset));

            if ui_path.is_empty() || ui_path == "/" {
                // Serve at root - use fallback so other routes take precedence
                app.fallback_service(embedded_routes)
            } else {
                // Serve at a specific path - convert to service for nesting
                app.nest_service(ui_path, embedded_routes)
            }
        }
        #[cfg(not(feature = "embed-ui"))]
        AssetSource::Embedded => {
            tracing::warn!(
                "Embedded UI assets requested but 'embed-ui' feature is not enabled, skipping"
            );
            app
        }
        AssetSource::Cdn { base_url } => {
            tracing::info!(base_url = %base_url, "UI assets served from CDN (no server-side serving)");
            app
        }
    }
}

/// Handler for serving embedded docs assets
#[cfg(feature = "embed-docs")]
async fn serve_docs_embedded_asset(
    axum::extract::Path(path): axum::extract::Path<String>,
) -> impl IntoResponse {
    serve_docs_embedded_file(&path)
}

/// Handler for serving embedded docs index at root
#[cfg(feature = "embed-docs")]
async fn serve_docs_embedded_index() -> Response {
    serve_docs_embedded_file("index.html")
}

/// Serve a file from the embedded docs assets.
/// Unlike the SPA UI, docs use static site routing:
/// - Try exact path first
/// - If path ends with /, try path + index.html
/// - If path doesn't end with /, try path/index.html
/// - Return 404 if not found (no fallback to root index.html)
#[cfg(feature = "embed-docs")]
fn serve_docs_embedded_file(path: &str) -> Response {
    // Try exact path first
    if let Some(content) = DocsAssets::get(path) {
        return build_docs_response(content);
    }

    // For paths ending with /, try index.html
    if path.ends_with('/') {
        let index_path = format!("{}index.html", path);
        if let Some(content) = DocsAssets::get(&index_path) {
            return build_docs_response(content);
        }
    } else {
        // For paths without trailing slash, try path/index.html
        let index_path = format!("{}/index.html", path);
        if let Some(content) = DocsAssets::get(&index_path) {
            return build_docs_response(content);
        }
    }

    // Return 404
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(Body::from("Not Found"))
        .unwrap()
}

#[cfg(feature = "embed-docs")]
fn build_docs_response(content: rust_embed::EmbeddedFile) -> Response {
    let content_type = content.metadata.mimetype();
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .body(Body::from(content.data.into_owned()))
        .unwrap()
}

/// Add routes for serving static documentation files
#[cfg(feature = "server")]
fn add_docs_routes(app: Router<AppState>, config: &config::GatewayConfig) -> Router<AppState> {
    use config::AssetSource;

    let docs_path = config.docs.path.trim_end_matches('/');

    match &config.docs.assets.source {
        #[cfg(feature = "server")]
        AssetSource::Filesystem { path } => {
            let assets_path = std::path::Path::new(path);

            if !assets_path.exists() {
                tracing::warn!(path = %path, "Documentation assets directory does not exist");
                return app;
            }

            tracing::info!(path = %path, docs_path = %docs_path, "Serving documentation from filesystem");

            // ServeDir without SPA fallback (static site)
            let serve_dir = ServeDir::new(path);

            // Add cache-control header for assets
            let cache_control = config.docs.assets.cache_control.clone();
            let serve_dir_with_headers = tower::ServiceBuilder::new()
                .layer(SetResponseHeaderLayer::if_not_present(
                    header::CACHE_CONTROL,
                    header::HeaderValue::from_str(&cache_control).unwrap_or_else(|_| {
                        header::HeaderValue::from_static("public, max-age=3600")
                    }),
                ))
                .service(serve_dir);

            // Docs are always at a specific path (never root)
            app.nest_service(docs_path, serve_dir_with_headers)
        }
        #[cfg(not(feature = "server"))]
        AssetSource::Filesystem { .. } => {
            tracing::warn!(
                "Filesystem docs assets requested but 'server' feature is not enabled, skipping"
            );
            app
        }
        #[cfg(feature = "embed-docs")]
        AssetSource::Embedded => {
            tracing::info!(docs_path = %docs_path, "Serving documentation from embedded assets");

            // Create routes for embedded assets (stateless)
            let embedded_routes: Router<()> = Router::new()
                .route("/", get(serve_docs_embedded_index))
                .route("/{*path}", get(serve_docs_embedded_asset));

            // Docs are always at a specific path (never root)
            app.nest_service(docs_path, embedded_routes)
        }
        #[cfg(not(feature = "embed-docs"))]
        AssetSource::Embedded => {
            tracing::warn!(
                "Embedded docs assets requested but 'embed-docs' feature is not enabled, skipping"
            );
            app
        }
        AssetSource::Cdn { base_url } => {
            tracing::info!(base_url = %base_url, "Documentation assets served from CDN (no server-side serving)");
            app
        }
    }
}

#[derive(Clone)]
pub struct AppState {
    pub http_client: Client,
    pub config: Arc<config::GatewayConfig>,
    pub db: Option<Arc<db::DbPool>>,
    pub services: Option<services::Services>,
    pub cache: Option<Arc<dyn cache::Cache>>,
    pub secrets: Option<Arc<dyn secrets::SecretManager>>,
    pub dlq: Option<Arc<dyn dlq::DeadLetterQueue>>,
    pub pricing: Arc<pricing::PricingConfig>,
    /// Registry of circuit breakers for providers.
    /// Shared across requests to persist failure tracking.
    pub circuit_breakers: providers::CircuitBreakerRegistry,
    /// Registry of provider health check states.
    /// Updated by background health checker, queried by admin API.
    pub provider_health: jobs::ProviderHealthStateRegistry,
    /// Task tracker for background tasks (usage logging, etc.)
    /// Ensures all spawned tasks complete during graceful shutdown.
    #[cfg(feature = "server")]
    pub task_tracker: TaskTracker,
    /// Bounded channel + drainer for partial-usage logging from
    /// `UsageTrackingStream::Drop`, which can fire outside a runtime context
    /// (so it cannot safely spawn tasks of its own).
    #[cfg(feature = "server")]
    pub usage_drain: streaming::UsageDrainHandle,
    /// Registry of per-organization OIDC authenticators.
    /// Loaded from org_sso_configs table at startup for multi-tenant SSO.
    #[cfg(feature = "sso")]
    pub oidc_registry: Option<Arc<auth::OidcAuthenticatorRegistry>>,
    /// Registry of per-organization SAML authenticators.
    /// Loaded from org_sso_configs table at startup for multi-tenant SSO.
    #[cfg(feature = "saml")]
    pub saml_registry: Option<Arc<auth::SamlAuthenticatorRegistry>>,
    /// Registry of per-org gateway JWT validators.
    /// Routes incoming JWTs to the correct org-scoped validator by issuer.
    #[cfg(feature = "jwt")]
    pub gateway_jwt_registry: Option<Arc<auth::GatewayJwtRegistry>>,
    /// Registry of per-organization RBAC policies.
    /// Loaded from org_rbac_policies table at startup for per-org authorization.
    pub policy_registry: Option<Arc<authz::PolicyRegistry>>,
    /// Async buffer for usage log entries.
    /// Batches writes to reduce database pressure.
    #[cfg(feature = "concurrency")]
    pub usage_buffer: Option<Arc<usage_buffer::UsageLogBuffer>>,
    /// Response cache for chat completions.
    /// Caches deterministic responses to reduce latency and costs.
    pub response_cache: Option<Arc<cache::ResponseCache>>,
    /// Semantic cache for chat completions.
    /// Uses vector similarity to find cached responses for semantically similar requests.
    pub semantic_cache: Option<Arc<cache::SemanticCache>>,
    /// Input guardrails evaluator for pre-request content filtering.
    /// Evaluates user input against guardrails policies before sending to the LLM.
    pub input_guardrails: Option<Arc<guardrails::InputGuardrails>>,
    /// Output guardrails evaluator for post-response content filtering.
    /// Evaluates LLM output against guardrails policies before returning to the user.
    pub output_guardrails: Option<Arc<guardrails::OutputGuardrails>>,
    /// Event bus for broadcasting server events to WebSocket subscribers.
    /// Used for real-time monitoring dashboards and push notifications.
    pub event_bus: Arc<events::EventBus>,
    /// File search service for RAG (Retrieval Augmented Generation).
    /// Used by the file_search tool in the Responses API to search vector stores.
    pub file_search_service: Option<Arc<services::FileSearchService>>,
    /// Shell tool runtime adapter. Constructed once at startup from
    /// `[features.shell]` config. `None` when shell tool is disabled.
    /// When the runtime advertises `passthrough_only`, the orchestrator
    /// skips registering a ShellExecutor and the shell tool flows
    /// through to the upstream provider unchanged.
    #[cfg(feature = "server")]
    pub shell_runtime: Option<Arc<dyn runtimes::ShellRuntime>>,
    /// MCP-tool service. Holds the pooled MCP clients and tools-list
    /// cache used by the `hadrian_hosted` mode. `None` when the `mcp`
    /// cargo feature is off or `[features.mcp]` is not configured.
    #[cfg(feature = "mcp")]
    pub mcp_service: Option<services::mcp::McpService>,
    /// Embedding service for Hadrian-side MCP tool search (semantic /
    /// hybrid ranking). Resolved from `[features.mcp.tool_search.embedding]`
    /// with a fallback to the file_search / semantic-cache embedding
    /// config. `None` when no embedding provider resolves — tool search
    /// then falls back to lexical ranking.
    #[cfg(feature = "mcp")]
    pub tool_search_embeddings: Option<Arc<cache::EmbeddingService>>,
    /// Persisted Responses API store. Always present when a database
    /// is configured; powers `GET/POST cancel/DELETE /v1/responses/{id}`
    /// and the cancellation signal pipeline.
    #[cfg(feature = "server")]
    pub responses_store: Option<Arc<services::ResponsesStore>>,
    /// Persisted Videos API store. Present when a database is configured;
    /// powers the `/v1/videos/*` proxy-on-read routing map.
    #[cfg(feature = "server")]
    pub video_store: Option<Arc<services::VideoStore>>,
    /// Containers service. Present when a database is configured;
    /// drives write-through persistence for the shell-tool
    /// `/mnt/data` artifact pipeline and serves
    /// `GET /v1/containers/*`.
    #[cfg(feature = "server")]
    pub containers_service: Option<Arc<services::containers::ContainersService>>,
    /// In-memory registry of live container sessions, keyed by the
    /// `cntr_…` id. Always present so cross-response container reuse
    /// works even in DB-less deployments (it just stays empty there).
    #[cfg(feature = "server")]
    pub container_session_registry: Arc<services::container_session::ContainerSessionRegistry>,
    /// Bounded-channel writer that batches `response_events` rows.
    /// Constructed alongside `responses_store` so persistence and event
    /// log share the same DB lifecycle.
    #[cfg(feature = "server")]
    pub response_event_buffer: Option<Arc<services::ResponseEventBuffer>>,
    /// Document processor for chunking and embedding files added to vector stores.
    /// Used by the Vector Store Files API to process uploaded files.
    #[cfg(any(
        feature = "document-extraction-basic",
        feature = "document-extraction-full"
    ))]
    pub document_processor: Option<Arc<services::DocumentProcessor>>,
    /// Default user ID for when auth is disabled.
    /// Created on startup to allow anonymous users to create conversations.
    pub default_user_id: Option<uuid::Uuid>,
    /// Default organization ID for when auth is disabled.
    /// Created on startup to allow anonymous users to create projects.
    pub default_org_id: Option<uuid::Uuid>,
    /// Provider metrics service for querying LLM provider statistics.
    /// Uses Prometheus when configured, or local /metrics parsing for single-node.
    pub provider_metrics: Arc<services::ProviderMetricsService>,
    /// Model catalog registry for enriching API responses with model metadata.
    /// Loaded from embedded data at startup and optionally synced at runtime.
    pub model_catalog: catalog::ModelCatalogRegistry,
    /// In-memory cache of model lists fetched from static (config-file) providers.
    /// Warmed on startup and refreshed periodically to avoid per-request latency.
    pub static_models_cache:
        Arc<tokio::sync::RwLock<std::collections::HashMap<String, providers::ModelsResponse>>>,
}

impl AppState {
    pub async fn new(config: config::GatewayConfig) -> Result<Self, Box<dyn std::error::Error>> {
        // Build a single shared HTTP client for all outbound provider requests.
        // This is efficient because reqwest maintains per-host connection pools internally,
        // so each provider (OpenAI, Anthropic, etc.) gets its own pool of connections.
        // See HttpClientConfig docs for architecture details and tuning options.
        let http_client = config
            .server
            .http_client
            .build_client()
            .map_err(|e| format!("Failed to build HTTP client: {}", e))?;

        tracing::debug!(
            timeout_secs = config.server.http_client.timeout_secs,
            connect_timeout_secs = config.server.http_client.connect_timeout_secs,
            pool_max_idle_per_host = config.server.http_client.pool_max_idle_per_host,
            http2_prior_knowledge = config.server.http_client.http2_prior_knowledge,
            "HTTP client configured"
        );

        // Initialize event bus early so it can be passed to services
        // Use channel capacity from WebSocket config
        let event_bus = Arc::new(events::EventBus::with_capacity(
            config.features.websocket.channel_capacity,
        ));

        // Initialize database and services if configured
        #[allow(unreachable_patterns)]
        let (db, mut services) = match &config.database {
            config::DatabaseConfig::None => (None, None),
            _ => {
                let pool = db::DbPool::from_config(&config.database).await?;
                // Run migrations on startup
                pool.run_migrations().await?;
                let db = Arc::new(pool);

                // Create file storage backend from config
                let file_storage = services::create_file_storage(&config.storage.files, db.clone())
                    .await
                    .map_err(|e| format!("Failed to initialize file storage: {}", e))?;

                tracing::info!(
                    backend = %file_storage.backend_name(),
                    "File storage backend initialized"
                );

                let max_expr_len = config.auth.rbac.max_expression_length;
                let max_skill_bytes = config.limits.resource_limits.max_skill_bytes;
                let services = services::Services::with_event_bus(
                    db.clone(),
                    file_storage,
                    event_bus.clone(),
                    max_expr_len,
                    max_skill_bytes,
                );
                (Some(db), Some(services))
            }
        };

        // Initialize cache if configured
        let cache: Option<Arc<dyn cache::Cache>> = match &config.cache {
            config::CacheConfig::None => None,
            config::CacheConfig::Memory(cfg) => Some(Arc::new(cache::MemoryCache::new(cfg))),
            config::CacheConfig::Redis(cfg) => {
                #[cfg(feature = "redis")]
                {
                    Some(Arc::new(cache::RedisCache::from_config(cfg).await?))
                }
                #[cfg(not(feature = "redis"))]
                {
                    let _ = cfg;
                    return Err("Redis cache configured but 'redis' feature not enabled. \
                        Rebuild with: cargo build --features redis"
                        .into());
                }
            }
        };

        // Wire the cache into services that benefit from a shared backend.
        // OAuth PKCE uses it for the per-code failure counter that burns a
        // code after repeated bad verifiers; without a cache it falls back
        // to the legacy "never burn on failure" behaviour.
        if let Some(services) = services.as_mut() {
            services.oauth_pkce = std::mem::replace(
                &mut services.oauth_pkce,
                services::OAuthPkceService::new(
                    db.clone()
                        .expect("services exist only when db is configured"),
                ),
            )
            .with_cache(cache.clone());

            // SCIM tokens get HMAC-SHA256 hashed with a pepper so that an
            // attacker who exfiltrates the database alone can't brute-force
            // them. We derive the pepper from the configured session secret
            // when one exists; otherwise we fall back to plain SHA-256 (and
            // log so operators know to set a session secret).
            #[cfg(feature = "sso")]
            {
                let pepper = config
                    .auth
                    .session
                    .as_ref()
                    .and_then(|s| s.secret.as_ref())
                    .map(|secret| secret.as_bytes().to_vec());
                if pepper.is_none() {
                    tracing::warn!(
                        "[auth.session].secret is not set — SCIM tokens will be stored as \
                         unsalted SHA-256. Configure a session secret to enable HMAC peppering."
                    );
                }
                services.scim_configs = std::mem::replace(
                    &mut services.scim_configs,
                    services::OrgScimConfigService::new(
                        db.clone()
                            .expect("services exist only when db is configured"),
                    ),
                )
                .with_token_pepper(pepper);
            }
        }

        // Initialize secrets manager based on configuration
        let secrets: Arc<dyn secrets::SecretManager> = match &config.secrets {
            config::SecretsConfig::None => {
                // Default behavior: use env vars for local mode, memory for db mode
                if db.is_some() {
                    tracing::warn!(
                        "No secrets manager configured. Using in-memory storage which does NOT \
                         persist across restarts. Per-org SSO will fail after restart. \
                         Configure [secrets] in hadrian.toml for production use."
                    );
                    Arc::new(secrets::MemorySecretManager::new())
                } else {
                    Arc::new(secrets::EnvSecretManager)
                }
            }
            config::SecretsConfig::Env => Arc::new(secrets::EnvSecretManager),
            #[cfg(feature = "vault")]
            config::SecretsConfig::Vault(vault_config) => {
                use config::VaultAuth;
                use secrets::SecretManager;

                // Build VaultConfig based on auth method
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
                        // Read the ServiceAccount token from the file
                        let jwt = std::fs::read_to_string(token_path).map_err(|e| {
                            format!(
                                "Failed to read Kubernetes ServiceAccount token from '{}': {}",
                                token_path, e
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
                    .map_err(|e| format!("Failed to create Vault client: {}", e))?;

                // Verify connectivity on startup
                manager
                    .health_check()
                    .await
                    .map_err(|e| format!("Vault health check failed: {}", e))?;

                let auth_method = match &vault_config.auth {
                    VaultAuth::Token { .. } => "token",
                    VaultAuth::AppRole { .. } => "approle",
                    VaultAuth::Kubernetes { .. } => "kubernetes",
                };

                tracing::info!(
                    address = %vault_config.address,
                    mount = %vault_config.mount,
                    path_prefix = %vault_config.path_prefix,
                    auth_method = %auth_method,
                    "Connected to Vault secrets manager"
                );

                Arc::new(manager)
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
                    .map_err(|e| format!("Failed to create AWS Secrets Manager client: {}", e))?;

                // Verify connectivity on startup
                manager
                    .health_check()
                    .await
                    .map_err(|e| format!("AWS Secrets Manager health check failed: {}", e))?;

                tracing::info!(
                    region = ?aws_config.region,
                    prefix = %aws_config.prefix,
                    "Connected to AWS Secrets Manager"
                );

                Arc::new(manager)
            }
            #[cfg(feature = "secrets-azure")]
            config::SecretsConfig::Azure(azure_config) => {
                use secrets::SecretManager;

                let cfg = secrets::AzureKeyVaultConfig::new(&azure_config.vault_url)
                    .with_prefix(&azure_config.prefix);

                let manager = secrets::AzureKeyVaultManager::new(cfg)
                    .await
                    .map_err(|e| format!("Failed to create Azure Key Vault client: {}", e))?;

                // Verify connectivity on startup
                manager
                    .health_check()
                    .await
                    .map_err(|e| format!("Azure Key Vault health check failed: {}", e))?;

                tracing::info!(
                    vault_url = %azure_config.vault_url,
                    prefix = %azure_config.prefix,
                    "Connected to Azure Key Vault"
                );

                Arc::new(manager)
            }
            #[cfg(feature = "secrets-gcp")]
            config::SecretsConfig::Gcp(gcp_config) => {
                use secrets::SecretManager;

                let cfg = secrets::GcpSecretManagerConfig::new(&gcp_config.project_id)
                    .with_prefix(&gcp_config.prefix);

                let manager = secrets::GcpSecretManager::new(cfg)
                    .await
                    .map_err(|e| format!("Failed to create GCP Secret Manager client: {}", e))?;

                // Verify connectivity on startup
                manager
                    .health_check()
                    .await
                    .map_err(|e| format!("GCP Secret Manager health check failed: {}", e))?;

                tracing::info!(
                    project_id = %gcp_config.project_id,
                    prefix = %gcp_config.prefix,
                    "Connected to GCP Secret Manager"
                );

                Arc::new(manager)
            }
        };

        // Initialize model catalog registry from embedded data (if available)
        let model_catalog = catalog::ModelCatalogRegistry::new();
        match catalog::embedded_catalog() {
            Some(json) => match model_catalog.load_from_json(&json) {
                Ok(()) => {
                    tracing::info!(
                        model_count = model_catalog.model_count(),
                        "Loaded embedded model catalog"
                    );
                }
                Err(e) => {
                    tracing::error!(error = %e, "Failed to parse embedded model catalog");
                }
            },
            None => {
                tracing::info!(
                    "No embedded model catalog available; \
                     enable the 'embed-catalog' feature or configure runtime sync"
                );
            }
        }

        // Initialize pricing from defaults + config + provider configs + catalog
        let pricing = Arc::new(pricing::PricingConfig::from_config_with_catalog(
            &config.pricing,
            &config.providers,
            Some(&model_catalog),
        ));

        // Initialize dead-letter queue if configured
        let dlq = dlq::create_dlq(&config.observability.dead_letter_queue, db.as_ref())
            .await
            .map_err(|e| format!("Failed to initialize DLQ: {}", e))?;

        if dlq.is_some() {
            tracing::info!("Dead-letter queue initialized");
        }

        // Initialize circuit breaker registry from provider config
        let circuit_breakers = providers::CircuitBreakerRegistry::from_config_with_event_bus(
            &config.providers,
            event_bus.clone(),
        );

        // Get session config from UI auth config
        // Note: Global OIDC config has been removed. Session config is used for per-org SSO.
        #[cfg(feature = "sso")]
        let session_config = config.auth.session.clone().unwrap_or_default();

        // Initialize per-org OIDC authenticator registry from database
        // This replaces the global OIDC authenticator
        #[cfg(feature = "sso")]
        let oidc_registry = if let Some(ref svc) = services {
            // Create session store for org authenticators (shared across all orgs)
            let enhanced = session_config.enhanced.enabled;
            let session_store = auth::create_session_store_with_enhanced(cache.clone(), enhanced);

            // Get default session config
            let default_session_config = session_config.clone();

            // No default redirect URI - per-org SSO configs must specify their own
            let default_redirect_uri: Option<String> = None;

            let url_validation_opts = crate::validation::UrlValidationOptions {
                allow_loopback: config.server.allow_loopback_urls,
                allow_private: config.server.allow_private_urls,
            };

            match auth::OidcAuthenticatorRegistry::initialize_from_db(
                &svc.org_sso_configs,
                secrets.as_ref(),
                session_store.clone(),
                default_session_config.clone(),
                default_redirect_uri.clone(),
                url_validation_opts,
            )
            .await
            {
                Ok(registry) => {
                    let count = registry.len().await;
                    if count > 0 {
                        tracing::info!(count, "Per-org SSO authenticator registry initialized");
                    } else {
                        tracing::debug!("Per-org SSO registry initialized (empty, will lazy load)");
                    }
                    // Always create the registry to support lazy loading from database
                    Some(Arc::new(registry))
                }
                Err(e) => {
                    // Create an empty registry instead of None - this allows lazy loading
                    // to work when requests come in, even if startup initialization failed
                    tracing::warn!(
                        error = %e,
                        "Failed to initialize org SSO registry from database, \
                         creating empty registry for lazy loading"
                    );
                    let empty_registry = auth::OidcAuthenticatorRegistry::new(
                        session_store,
                        default_session_config,
                        default_redirect_uri,
                        url_validation_opts,
                    );
                    Some(Arc::new(empty_registry))
                }
            }
        } else {
            None
        };

        // Initialize per-org SAML authenticator registry from database
        #[cfg(feature = "saml")]
        let saml_registry = if let Some(ref svc) = services {
            // Create session store for org authenticators (shared across all orgs)
            let enhanced = session_config.enhanced.enabled;
            let session_store = auth::create_session_store_with_enhanced(cache.clone(), enhanced);

            // Get default session config
            let default_session_config = session_config.clone();

            // Build default ACS URL from server config
            let default_acs_url = format!(
                "{}://{}:{}/auth/saml/acs",
                if config.server.tls.is_some() {
                    "https"
                } else {
                    "http"
                },
                config.server.host,
                config.server.port
            );

            match auth::SamlAuthenticatorRegistry::initialize_from_db(
                &svc.org_sso_configs,
                secrets.as_ref(),
                session_store,
                default_session_config,
                default_acs_url,
            )
            .await
            {
                Ok(registry) if !registry.is_empty().await => {
                    tracing::info!(
                        count = registry.len().await,
                        "Per-org SAML authenticator registry initialized"
                    );
                    Some(Arc::new(registry))
                }
                Ok(_) => {
                    tracing::debug!("No SAML org SSO configs found, registry empty");
                    None
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to initialize SAML org SSO registry");
                    None
                }
            }
        } else {
            None
        };

        // Initialize per-org gateway JWT registry for multi-tenant JWT auth on /v1/*.
        // Validators are pre-loaded in a background task so server startup isn't
        // blocked by N sequential OIDC discovery HTTP requests.
        #[cfg(feature = "jwt")]
        let gateway_jwt_registry = if db.is_some() {
            Some(Arc::new(auth::GatewayJwtRegistry::new()))
        } else {
            None
        };

        // Initialize per-org RBAC policy registry from database
        let policy_registry = if let (Some(svc), Some(db_pool)) = (&services, &db)
            && config.auth.rbac.enabled
        {
            let engine = Arc::new(
                authz::AuthzEngine::new(config.auth.rbac.clone())
                    .expect("Failed to create AuthzEngine for policy registry"),
            );

            // Get config values for the registry
            let version_check_ttl =
                std::time::Duration::from_millis(config.auth.rbac.policy_cache_ttl_ms);
            let max_cached_orgs = config.auth.rbac.max_cached_orgs;
            let eviction_batch_size = config.auth.rbac.policy_eviction_batch_size;

            if config.auth.rbac.lazy_load_policies {
                // Lazy loading: policies loaded on-demand when org is first accessed
                let registry = authz::PolicyRegistry::new_lazy(
                    engine,
                    config.auth.rbac.default_effect,
                    cache.clone(),
                    db_pool.org_rbac_policies(),
                    version_check_ttl,
                    max_cached_orgs,
                    eviction_batch_size,
                );
                tracing::info!(
                    max_cached_orgs,
                    eviction_batch_size,
                    "RBAC policy registry initialized (lazy loading)"
                );
                Some(Arc::new(registry))
            } else {
                // Eager loading: load all policies at startup
                match authz::PolicyRegistry::initialize_from_db(
                    &svc.org_rbac_policies,
                    engine,
                    config.auth.rbac.default_effect,
                    cache.clone(),
                    db_pool.org_rbac_policies(),
                    version_check_ttl,
                    max_cached_orgs,
                    eviction_batch_size,
                )
                .await
                {
                    Ok(registry) => {
                        let org_count = registry.org_count().await;
                        let policy_count = registry.policy_count().await;
                        if org_count > 0 {
                            tracing::info!(
                                org_count,
                                policy_count,
                                max_cached_orgs,
                                "RBAC policy registry initialized (eager loading)"
                            );
                        } else {
                            tracing::debug!("RBAC policy registry initialized (no org policies)");
                        }
                        Some(Arc::new(registry))
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Failed to initialize RBAC policy registry");
                        None
                    }
                }
            }
        } else {
            None
        };

        // Initialize usage log buffer with configured buffer settings and EventBus
        #[cfg(feature = "concurrency")]
        let usage_buffer = {
            let buffer_config =
                usage_buffer::UsageBufferConfig::from(&config.observability.usage.buffer);
            let buffer = Arc::new(usage_buffer::UsageLogBuffer::with_event_bus(
                buffer_config,
                event_bus.clone(),
            ));
            Some(buffer)
        };

        // Initialize response cache if configured and cache is available
        let response_cache = match (&config.features.response_caching, &cache) {
            (Some(caching_config), Some(cache_instance)) if caching_config.enabled => {
                tracing::info!(
                    ttl_secs = caching_config.ttl_secs,
                    only_deterministic = caching_config.only_deterministic,
                    max_size_bytes = caching_config.max_size_bytes,
                    "Response caching enabled"
                );
                Some(Arc::new(cache::ResponseCache::new(
                    cache_instance.clone(),
                    caching_config.clone(),
                )))
            }
            (Some(caching_config), None) if caching_config.enabled => {
                tracing::warn!(
                    "Response caching is enabled but no cache backend is configured. \
                     Add [cache] configuration to enable response caching."
                );
                None
            }
            _ => None,
        };

        // Create the task tracker for background tasks
        #[cfg(feature = "server")]
        let task_tracker = TaskTracker::new();
        // Bounded usage-drain channel + drainer task. Owned by the same
        // tracker so graceful shutdown waits for it to finish flushing.
        #[cfg(feature = "server")]
        let usage_drain =
            streaming::UsageDrainHandle::spawn(&task_tracker, streaming::USAGE_DRAIN_CAPACITY);

        // Initialize semantic cache if configured
        #[cfg(feature = "server")]
        let semantic_cache = Self::init_semantic_cache(
            &config,
            cache.as_ref(),
            db.as_ref(),
            &circuit_breakers,
            http_client.clone(),
            &task_tracker,
        )
        .await;
        #[cfg(not(feature = "server"))]
        let semantic_cache: Option<Arc<cache::SemanticCache>> = None;

        // Initialize input guardrails if configured
        let input_guardrails = match &config.features.guardrails {
            Some(guardrails_config) => {
                match guardrails::InputGuardrails::from_config(guardrails_config, &http_client) {
                    Ok(Some(evaluator)) => {
                        tracing::info!(
                            provider = %evaluator.provider_name(),
                            "Input guardrails enabled"
                        );
                        Some(Arc::new(evaluator))
                    }
                    Ok(None) => {
                        tracing::debug!("Input guardrails disabled or not configured");
                        None
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Failed to initialize input guardrails");
                        None
                    }
                }
            }
            None => None,
        };

        // Initialize output guardrails if configured
        let output_guardrails = match &config.features.guardrails {
            Some(guardrails_config) => {
                match guardrails::OutputGuardrails::from_config(guardrails_config, &http_client) {
                    Ok(Some(evaluator)) => {
                        tracing::info!(
                            provider = %evaluator.provider_name(),
                            "Output guardrails enabled"
                        );
                        Some(Arc::new(evaluator))
                    }
                    Ok(None) => {
                        tracing::debug!("Output guardrails disabled or not configured");
                        None
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Failed to initialize output guardrails");
                        None
                    }
                }
            }
            None => None,
        };

        // Initialize file search service if configured
        // This requires both semantic cache components (embedding service + vector store)
        // and file_search configuration
        let file_search_service = Self::init_file_search_service(
            &config,
            db.as_ref(),
            &circuit_breakers,
            http_client.clone(),
        )
        .await;

        // Initialize the persisted Responses API store when a database
        // is available. Requests without a DB run stateless — shell
        // tool retrieval/cancel/delete endpoints will 404.
        #[cfg(feature = "server")]
        let responses_store: Option<Arc<services::ResponsesStore>> = db.as_ref().map(|db| {
            let mut store = services::ResponsesStore::new(
                db.clone(),
                std::time::Duration::from_secs(config.features.responses.retention_secs),
            );
            if let Some(ref hook) = config.features.responses.webhook {
                let dispatcher = services::ResponsesWebhookDispatcher::spawn(
                    hook.clone(),
                    http_client.clone(),
                    dlq.clone(),
                );
                store = store.with_webhook(dispatcher);
                tracing::info!(url = %hook.url, "Responses webhook configured");
            }
            Arc::new(store)
        });

        // Initialize the persisted Videos API store when a database is
        // available. Without a DB the `/v1/videos/*` endpoints 404 (the
        // proxy-on-read routing map has nowhere to live).
        #[cfg(feature = "server")]
        let video_store: Option<Arc<services::VideoStore>> = db.as_ref().map(|db| {
            Arc::new(services::VideoStore::new(
                db.clone(),
                std::time::Duration::from_secs(config.features.videos.retention_secs),
            ))
        });

        // Event buffer writes batched response_events. Defaults: 100ms
        // flush interval, batches of 64 events, channel of 1024.
        #[cfg(feature = "server")]
        let response_event_buffer: Option<Arc<services::ResponseEventBuffer>> =
            db.as_ref().map(|db| {
                Arc::new(services::ResponseEventBuffer::spawn(
                    db.response_events(),
                    64,
                    std::time::Duration::from_millis(100),
                    1024,
                ))
            });

        // Containers service powers shell-tool `/mnt/data` artifact
        // persistence and `/v1/containers/*`. Available whenever a DB
        // is configured. Without it the live shell tool still works
        // (the in-memory session capture path stays available), but
        // the GET endpoints return 404 because no rows exist.
        #[cfg(feature = "server")]
        let containers_service: Option<Arc<services::containers::ContainersService>> =
            match db.as_ref() {
                Some(db) => {
                    // Container artifacts get their own storage backend
                    // (`[storage.container_files]`) so operators can offload
                    // bulky `/mnt/data` outputs to filesystem / S3 while
                    // keeping the Files API wherever they like.
                    let container_file_storage =
                        services::create_file_storage(&config.storage.container_files, db.clone())
                            .await
                            .map_err(|e| {
                                format!("Failed to initialize container file storage: {}", e)
                            })?;
                    tracing::info!(
                        backend = %container_file_storage.backend_name(),
                        "Container file storage backend initialized"
                    );
                    Some(Arc::new(services::containers::ContainersService::new(
                        db.clone(),
                        container_file_storage,
                    )))
                }
                None => None,
            };

        // Always construct a registry. In DB-less deployments it
        // stays empty (sessions never get inserted), but wiring it in
        // unconditionally keeps the rest of the pipeline's plumbing
        // simple.
        #[cfg(feature = "server")]
        let container_session_registry: Arc<
            services::container_session::ContainerSessionRegistry,
        > = Arc::new(services::container_session::ContainerSessionRegistry::new());

        // Initialize the shell tool runtime from [features.shell].
        // Microsandbox / OpenSandbox / E2B adapters land in slice 1B; for
        // now they return None and emit a clear startup error.
        #[cfg(feature = "server")]
        let shell_runtime: Option<Arc<dyn runtimes::ShellRuntime>> = match &config.features.shell {
            config::ShellRuntimeConfig::None => None,
            config::ShellRuntimeConfig::PassthroughOpenAI => {
                tracing::info!("Shell tool runtime: passthrough_openai");
                Some(Arc::new(
                    runtimes::PassthroughRuntime::for_openai_container(),
                ))
            }
            config::ShellRuntimeConfig::ClientPassthrough => {
                tracing::info!(
                    "Shell tool runtime: client_passthrough (API client fulfills shell calls)"
                );
                Some(Arc::new(runtimes::PassthroughRuntime::for_api_client()))
            }
            #[cfg(feature = "runtime-microsandbox")]
            config::ShellRuntimeConfig::Microsandbox(cfg) => {
                tracing::info!(
                    image = %cfg.image,
                    cpus = cfg.cpus,
                    memory_mb = cfg.memory_mb,
                    "Shell tool runtime: microsandbox"
                );
                Some(Arc::new(runtimes::MicrosandboxRuntime::new(cfg.clone())))
            }
            #[cfg(feature = "runtime-opensandbox")]
            config::ShellRuntimeConfig::OpenSandbox(cfg) => {
                tracing::info!(
                    endpoint = %cfg.endpoint,
                    "Shell tool runtime: opensandbox"
                );
                Some(Arc::new(runtimes::OpenSandboxRuntime::new(
                    cfg.clone(),
                    http_client.clone(),
                )))
            }
        };

        // MCP tool service. Built when `[features.mcp]` is configured;
        // the executor + preprocess pick it up off AppState. The
        // `hadrian_hosted` mode is the consumer; under
        // `passthrough_openai` the service is constructed but unused.
        #[cfg(feature = "mcp")]
        let mcp_service: Option<services::mcp::McpService> = match &config.features.mcp {
            Some(cfg) if cfg.enabled => {
                tracing::info!(
                    mode = ?cfg.mode,
                    "MCP tool: enabled"
                );
                let approvals_repo = db.as_ref().map(|db| db.mcp_pending_approvals());
                let url_validation_opts = crate::validation::UrlValidationOptions {
                    allow_loopback: config.server.allow_loopback_urls,
                    allow_private: config.server.allow_private_urls,
                };
                Some(services::mcp::McpService::with_approvals_repo(
                    approvals_repo,
                    url_validation_opts,
                ))
            }
            _ => None,
        };

        // Resolve the embedding service for Hadrian-side MCP tool search.
        #[cfg(feature = "mcp")]
        let tool_search_embeddings = Self::init_tool_search_embeddings(
            &config,
            &circuit_breakers,
            http_client.clone(),
            file_search_service.as_ref(),
        );

        // Initialize document processor for RAG file processing
        // This reuses the embedding service and vector store from file_search_service
        #[cfg(any(
            feature = "document-extraction-basic",
            feature = "document-extraction-full"
        ))]
        let document_processor = Self::init_document_processor(
            &config,
            db.as_ref(),
            services.as_ref(),
            file_search_service.as_ref(),
        );

        // Create default user and organization when auth is disabled (for anonymous access)
        let (default_user_id, default_org_id) = if !config.auth.is_auth_enabled() {
            if let Some(ref svc) = services {
                let user_id = match Self::ensure_default_user(svc).await {
                    Ok(id) => {
                        tracing::info!(user_id = %id, "Default anonymous user available");
                        Some(id)
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Failed to create default user");
                        None
                    }
                };

                let org_id = match Self::ensure_default_org(svc).await {
                    Ok(id) => {
                        tracing::info!(org_id = %id, "Default local organization available");
                        Some(id)
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Failed to create default organization");
                        None
                    }
                };

                // Add user to org if both exist
                if let (Some(uid), Some(oid)) = (user_id, org_id)
                    && let Err(e) = Self::ensure_default_org_membership(svc, uid, oid).await
                {
                    tracing::warn!(error = %e, "Failed to add user to default organization");
                }

                (user_id, org_id)
            } else {
                (None, None)
            }
        } else {
            (None, None)
        };

        // Initialize provider metrics service
        // Uses Prometheus API when prometheus_query_url is configured, otherwise local /metrics
        let provider_metrics = {
            #[cfg(feature = "prometheus")]
            {
                if let Some(ref prometheus_url) = config.observability.metrics.prometheus_query_url
                {
                    match services::ProviderMetricsService::with_prometheus(prometheus_url) {
                        Ok(svc) => {
                            tracing::info!(
                                prometheus_url = %prometheus_url,
                                "Provider metrics using Prometheus backend"
                            );
                            Arc::new(svc)
                        }
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                "Failed to create Prometheus client, falling back to local metrics"
                            );
                            Arc::new(services::ProviderMetricsService::from_prometheus_handle(
                                observability::metrics::get_prometheus_handle(),
                            ))
                        }
                    }
                } else {
                    tracing::info!("Provider metrics using local /metrics endpoint");
                    Arc::new(services::ProviderMetricsService::from_prometheus_handle(
                        observability::metrics::get_prometheus_handle(),
                    ))
                }
            }
            #[cfg(not(feature = "prometheus"))]
            Arc::new(services::ProviderMetricsService::new())
        };

        let result = Ok(Self {
            http_client,
            config: Arc::new(config),
            db,
            services,
            cache,
            secrets: Some(secrets),
            dlq,
            pricing,
            circuit_breakers,
            provider_health: jobs::ProviderHealthStateRegistry::new(),
            #[cfg(feature = "server")]
            task_tracker,
            #[cfg(feature = "server")]
            usage_drain,
            #[cfg(feature = "sso")]
            oidc_registry,
            #[cfg(feature = "saml")]
            saml_registry,
            #[cfg(feature = "jwt")]
            gateway_jwt_registry,
            policy_registry,
            #[cfg(feature = "concurrency")]
            usage_buffer,
            response_cache,
            semantic_cache,
            input_guardrails,
            output_guardrails,
            event_bus,
            file_search_service,
            #[cfg(feature = "server")]
            shell_runtime,
            #[cfg(feature = "mcp")]
            mcp_service,
            #[cfg(feature = "mcp")]
            tool_search_embeddings,
            #[cfg(feature = "server")]
            responses_store,
            #[cfg(feature = "server")]
            video_store,
            #[cfg(feature = "server")]
            containers_service,
            #[cfg(feature = "server")]
            container_session_registry,
            #[cfg(feature = "server")]
            response_event_buffer,
            #[cfg(any(
                feature = "document-extraction-basic",
                feature = "document-extraction-full"
            ))]
            document_processor,
            default_user_id,
            default_org_id,
            provider_metrics,
            model_catalog,
            static_models_cache: Arc::new(tokio::sync::RwLock::new(
                std::collections::HashMap::new(),
            )),
        });

        // Note: the static models cache is no longer warmed inside
        // `AppState::new`. The CLI server entrypoint spawns the warm on a
        // background task after the listener is bound so a slow/dead
        // provider can't delay startup or the readiness probe.
        result
    }

    /// Ensure a default user exists for anonymous access when auth is disabled.
    /// Uses a well-known external_id so the same user is used across restarts.
    /// Race-safe: tries to create first, falls back to lookup on conflict.
    pub(crate) async fn ensure_default_user(
        services: &services::Services,
    ) -> Result<uuid::Uuid, Box<dyn std::error::Error + Send + Sync>> {
        use crate::db::DbError;

        const ANONYMOUS_EXTERNAL_ID: &str = "anonymous";

        // Try to create first to avoid TOCTOU race between multiple instances
        let user = models::CreateUser {
            external_id: ANONYMOUS_EXTERNAL_ID.to_string(),
            email: Some("anonymous@localhost".to_string()),
            name: Some("Anonymous User".to_string()),
        };

        match services.users.create(user).await {
            Ok(created) => Ok(created.id),
            Err(DbError::Conflict(_)) => {
                // Already exists (created by another instance) -- look it up
                let existing = services
                    .users
                    .get_by_external_id(ANONYMOUS_EXTERNAL_ID)
                    .await?
                    .ok_or("Default user conflict but not found")?;
                Ok(existing.id)
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Ensure a default organization exists for anonymous access when auth is disabled.
    /// Uses a well-known slug so the same organization is used across restarts.
    /// Race-safe: tries to create first, falls back to lookup on conflict.
    pub(crate) async fn ensure_default_org(
        services: &services::Services,
    ) -> Result<uuid::Uuid, Box<dyn std::error::Error + Send + Sync>> {
        use crate::db::DbError;

        const LOCAL_ORG_SLUG: &str = "local";

        // Try to create first to avoid TOCTOU race between multiple instances
        let org = models::CreateOrganization {
            slug: LOCAL_ORG_SLUG.to_string(),
            name: "Local".to_string(),
        };

        match services.organizations.create(org).await {
            Ok(created) => Ok(created.id),
            Err(DbError::Conflict(_)) => {
                // Already exists (created by another instance) -- look it up
                let existing = services
                    .organizations
                    .get_by_slug(LOCAL_ORG_SLUG)
                    .await?
                    .ok_or("Default org conflict but not found")?;
                Ok(existing.id)
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Ensure the default user is a member of the default organization.
    pub(crate) async fn ensure_default_org_membership(
        services: &services::Services,
        user_id: uuid::Uuid,
        org_id: uuid::Uuid,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        use crate::{db::DbError, models::MembershipSource};
        // Try to add the user to the org - if they're already a member, this will fail
        // with a unique constraint violation which we can ignore
        match services
            .users
            .add_to_org(user_id, org_id, "member", MembershipSource::Manual)
            .await
        {
            Ok(()) => {
                tracing::debug!(user_id = %user_id, org_id = %org_id, "Added user to organization");
                Ok(())
            }
            Err(DbError::Conflict(_)) => {
                tracing::debug!(user_id = %user_id, org_id = %org_id, "User already member of organization");
                Ok(())
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Check if the gateway is in "full" mode (has database and cache)
    pub fn is_full_mode(&self) -> bool {
        self.db.is_some() && self.cache.is_some()
    }

    /// Initialize semantic cache if configured.
    ///
    /// Spawns the background embedding worker on the provided task tracker.
    #[cfg(feature = "server")]
    async fn init_semantic_cache(
        config: &config::GatewayConfig,
        cache: Option<&Arc<dyn cache::Cache>>,
        db: Option<&Arc<db::DbPool>>,
        circuit_breakers: &providers::CircuitBreakerRegistry,
        http_client: Client,
        task_tracker: &TaskTracker,
    ) -> Option<Arc<cache::SemanticCache>> {
        #[cfg(not(feature = "database-postgres"))]
        let _ = &db;
        // Check if semantic caching is configured
        let semantic_config = match &config.features.response_caching {
            Some(caching_config) if caching_config.enabled => match &caching_config.semantic {
                Some(semantic) if semantic.enabled => semantic,
                _ => return None,
            },
            _ => return None,
        };

        // Ensure we have a cache backend
        let cache_instance = match cache {
            Some(c) => c.clone(),
            None => {
                tracing::warn!(
                    "Semantic caching is enabled but no cache backend is configured. \
                     Add [cache] configuration to enable semantic caching."
                );
                return None;
            }
        };

        // Get the embedding provider configuration
        let provider_config = match config.providers.get(&semantic_config.embedding.provider) {
            Some(cfg) => cfg,
            None => {
                tracing::warn!(
                    provider = %semantic_config.embedding.provider,
                    "Semantic caching is enabled but embedding provider '{}' is not configured. \
                     Add the provider to [providers] configuration.",
                    semantic_config.embedding.provider
                );
                return None;
            }
        };

        // Create embedding service
        let embedding_service = match cache::EmbeddingService::new(
            &semantic_config.embedding,
            provider_config,
            circuit_breakers,
            http_client,
        ) {
            Ok(service) => Arc::new(service),
            Err(e) => {
                tracing::error!(
                    error = %e,
                    "Failed to create embedding service for semantic caching"
                );
                return None;
            }
        };

        // Create vector store based on configuration
        let vector_store: Arc<dyn cache::vector_store::VectorBackend> = match &semantic_config
            .vector_backend
        {
            #[cfg(feature = "database-postgres")]
            config::SemanticVectorBackend::Pgvector {
                table_name,
                index_type,
                distance_metric,
            } => {
                // Ensure we have a PostgreSQL database
                let pg_pool = match db.and_then(|d| d.pg_write_pool()) {
                    Some(pool) => pool.clone(),
                    None => {
                        tracing::warn!(
                            "Semantic caching with pgvector requires PostgreSQL database. \
                                 Configure [database] with type = \"postgres\"."
                        );
                        return None;
                    }
                };

                let store = cache::vector_store::PgvectorStore::new(
                    pg_pool,
                    table_name.clone(),
                    semantic_config.embedding.dimensions,
                    index_type.clone(),
                    *distance_metric,
                );

                // Initialize the pgvector table
                if let Err(e) = store.initialize().await {
                    tracing::error!(
                        error = %e,
                        "Failed to initialize pgvector store for semantic caching"
                    );
                    return None;
                }

                Arc::new(store)
            }
            #[cfg(not(feature = "database-postgres"))]
            config::SemanticVectorBackend::Pgvector { .. } => {
                tracing::warn!(
                    "Semantic caching with pgvector requires the 'database-postgres' feature. \
                         Rebuild with --features database-postgres or use a different vector backend."
                );
                return None;
            }
            config::SemanticVectorBackend::Qdrant {
                url,
                api_key,
                qdrant_collection_name,
                distance_metric,
            } => {
                let store = cache::vector_store::QdrantStore::new(
                    url.clone(),
                    api_key.clone(),
                    qdrant_collection_name.clone(),
                    semantic_config.embedding.dimensions,
                    *distance_metric,
                );

                // Initialize the Qdrant index
                if let Err(e) = store.initialize().await {
                    tracing::error!(
                        error = %e,
                        "Failed to initialize Qdrant store for semantic caching"
                    );
                    return None;
                }

                Arc::new(store)
            }
        };

        // Create the semantic cache with background worker
        let (semantic_cache, worker) = cache::SemanticCache::new(
            cache_instance,
            vector_store,
            embedding_service,
            semantic_config.clone(),
        );

        // Spawn the background embedding worker
        task_tracker.spawn(worker);

        tracing::info!(
            similarity_threshold = semantic_config.similarity_threshold,
            top_k = semantic_config.top_k,
            embedding_provider = %semantic_config.embedding.provider,
            embedding_model = %semantic_config.embedding.model,
            "Semantic caching enabled"
        );

        Some(Arc::new(semantic_cache))
    }

    /// Initialize file search service if configured.
    ///
    /// The file search service requires:
    /// - A database for vector store metadata
    /// - An embedding service for generating query embeddings
    /// - A vector store for similarity search
    ///
    /// The embedding configuration is taken from the semantic caching config if available,
    /// since file search typically uses the same embedding model.
    /// Resolve an embedding service for Hadrian-side MCP tool search.
    ///
    /// Only relevant under `hadrian_hosted`. Resolves the embedding config
    /// with priority: `[features.mcp.tool_search.embedding]` →
    /// `[features.file_search.embedding]` →
    /// `[features.response_caching.semantic.embedding]`; failing that,
    /// reuses the file_search embedding service if one was built. Returns
    /// `None` when nothing resolves (tool search falls back to lexical
    /// ranking). Logs loudly when the configured ranker is `semantic` but
    /// no embeddings resolve.
    #[cfg(feature = "mcp")]
    fn init_tool_search_embeddings(
        config: &config::GatewayConfig,
        circuit_breakers: &providers::CircuitBreakerRegistry,
        http_client: Client,
        file_search_service: Option<&Arc<services::FileSearchService>>,
    ) -> Option<Arc<cache::EmbeddingService>> {
        let mcp_cfg = match &config.features.mcp {
            Some(cfg) if cfg.enabled && cfg.is_hadrian_hosted() => cfg,
            _ => return None,
        };
        let ts_cfg = &mcp_cfg.tool_search;

        // Lexical ranking needs no embeddings.
        if ts_cfg.ranker == crate::api_types::responses::ToolSearchRankerKind::Lexical {
            return None;
        }

        let embedding_config = ts_cfg.embedding.as_ref().or_else(|| {
            config
                .features
                .file_search
                .as_ref()
                .and_then(|fs| fs.embedding.as_ref())
                .or_else(|| {
                    config
                        .features
                        .response_caching
                        .as_ref()
                        .and_then(|rc| rc.semantic.as_ref())
                        .map(|sc| &sc.embedding)
                })
        });

        let resolved = embedding_config.and_then(|cfg| {
            let provider_config = config.providers.get(&cfg.provider)?;
            match cache::EmbeddingService::new(
                cfg,
                provider_config,
                circuit_breakers,
                http_client.clone(),
            ) {
                Ok(service) => Some(Arc::new(service)),
                Err(e) => {
                    tracing::error!(error = %e, "Failed to build embedding service for MCP tool search");
                    None
                }
            }
        });

        // Last resort: reuse the file_search embedding service if present.
        let resolved = resolved.or_else(|| file_search_service.map(|fs| fs.embedding_service()));

        if resolved.is_none() {
            // `hybrid` degrades to lexical; `semantic` was explicitly asked
            // for and can't be honored — surface it loudly.
            if ts_cfg.ranker == crate::api_types::responses::ToolSearchRankerKind::Semantic {
                tracing::error!(
                    "MCP tool search ranker is `semantic` but no embedding provider resolved; \
                     configure [features.mcp.tool_search.embedding] (or file_search / semantic \
                     cache embeddings). Falling back to lexical ranking."
                );
            } else {
                tracing::info!(
                    "MCP tool search: no embedding provider resolved; using lexical ranking"
                );
            }
        }

        resolved
    }

    async fn init_file_search_service(
        config: &config::GatewayConfig,
        db: Option<&Arc<db::DbPool>>,
        circuit_breakers: &providers::CircuitBreakerRegistry,
        http_client: Client,
    ) -> Option<Arc<services::FileSearchService>> {
        // Check if file_search is enabled
        let file_search_config = match &config.features.file_search {
            Some(cfg) if cfg.enabled => cfg,
            _ => return None,
        };

        // File search requires a database
        let db = match db {
            Some(d) => d.clone(),
            None => {
                tracing::warn!(
                    "File search is enabled but no database is configured. \
                     Add [database] configuration to enable file search."
                );
                return None;
            }
        };

        // Get embedding configuration with priority:
        // 1. file_search.embedding (explicit RAG config)
        // 2. response_caching.semantic.embedding (semantic cache config)
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
                tracing::warn!(
                    "File search is enabled but no embedding configuration found. \
                     Configure [features.file_search.embedding] or \
                     [features.response_caching.semantic.embedding] to enable file search."
                );
                return None;
            }
        };

        // Get the embedding provider configuration
        let provider_config = match config.providers.get(&embedding_config.provider) {
            Some(cfg) => cfg,
            None => {
                tracing::warn!(
                    provider = %embedding_config.provider,
                    "File search is enabled but embedding provider '{}' is not configured. \
                     Add the provider to [providers] configuration.",
                    embedding_config.provider
                );
                return None;
            }
        };

        // Create embedding service
        let embedding_service = match cache::EmbeddingService::new(
            embedding_config,
            provider_config,
            circuit_breakers,
            http_client.clone(),
        ) {
            Ok(service) => Arc::new(service),
            Err(e) => {
                tracing::error!(
                    error = %e,
                    "Failed to create embedding service for file search"
                );
                return None;
            }
        };

        // Get vector backend configuration with priority:
        // 1. file_search.vector_backend (explicit RAG config - RECOMMENDED)
        // 2. response_caching.semantic.vector_backend (semantic cache config - for backward compat)
        // 3. Default pgvector with "rag_chunks" table
        //
        // Using separate vector storage for RAG ensures:
        // - RAG chunks are stored in clearly named tables (rag_chunks vs semantic_cache_embeddings)
        // - Independent configuration for RAG vs semantic caching
        // - No confusion about what data is in which table
        let vector_store: Arc<dyn cache::vector_store::VectorBackend> = if let Some(rag_backend) =
            &file_search_config.vector_backend
        {
            // Priority 1: Explicit RAG vector backend configuration
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
                            tracing::warn!(
                                "File search with pgvector requires PostgreSQL database. \
                                     Configure [database] with type = \"postgres\"."
                            );
                            return None;
                        }
                    };

                    // For RAG, the table_name IS the chunks table (not a prefix)
                    // We create a PgvectorStore but only use the chunks operations
                    let store = cache::vector_store::PgvectorStore::new(
                        pg_pool,
                        // Use a dummy name for semantic cache table since we won't use it
                        // The chunks table will be "{table_name}_chunks" but we want
                        // the table_name to BE the chunks table, so we strip "_chunks"
                        // if it's there, otherwise prepend with a prefix
                        format!("{}_semantic", table_name.trim_end_matches("_chunks")),
                        embedding_config.dimensions,
                        index_type.clone(),
                        *distance_metric,
                    );

                    if let Err(e) = store.initialize().await {
                        tracing::error!(
                            error = %e,
                            "Failed to initialize pgvector store for file search"
                        );
                        return None;
                    }

                    tracing::info!(
                        table_name = %table_name,
                        "RAG using dedicated pgvector table"
                    );

                    Arc::new(store)
                }
                #[cfg(not(feature = "database-postgres"))]
                config::RagVectorBackend::Pgvector { .. } => {
                    tracing::warn!(
                        "File search with pgvector requires the 'database-postgres' feature. \
                             Rebuild with --features database-postgres or use a different vector backend."
                    );
                    return None;
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
                        tracing::error!(
                            error = %e,
                            "Failed to initialize Qdrant store for file search"
                        );
                        return None;
                    }

                    tracing::info!(
                        collection_name = %qdrant_collection_name,
                        "RAG using dedicated Qdrant index"
                    );

                    Arc::new(store)
                }
            }
        } else if let Some(semantic_config) = config
            .features
            .response_caching
            .as_ref()
            .and_then(|rc| rc.semantic.as_ref())
        {
            // Priority 2: Fall back to semantic cache vector backend (backward compatibility)
            // Note: This shares tables with semantic cache, which may cause confusion
            tracing::info!(
                "RAG using semantic cache vector backend. Consider configuring \
                     [features.file_search.vector_backend] for dedicated RAG storage."
            );

            match &semantic_config.vector_backend {
                #[cfg(feature = "database-postgres")]
                config::SemanticVectorBackend::Pgvector {
                    table_name,
                    index_type,
                    distance_metric,
                } => {
                    let pg_pool = match db.pg_write_pool() {
                        Some(pool) => pool.clone(),
                        None => {
                            tracing::warn!(
                                "File search with pgvector requires PostgreSQL database. \
                                     Configure [database] with type = \"postgres\"."
                            );
                            return None;
                        }
                    };

                    let store = cache::vector_store::PgvectorStore::new(
                        pg_pool,
                        table_name.clone(),
                        embedding_config.dimensions,
                        index_type.clone(),
                        *distance_metric,
                    );

                    if let Err(e) = store.initialize().await {
                        tracing::error!(
                            error = %e,
                            "Failed to initialize pgvector store for file search"
                        );
                        return None;
                    }

                    Arc::new(store)
                }
                #[cfg(not(feature = "database-postgres"))]
                config::SemanticVectorBackend::Pgvector { .. } => {
                    tracing::warn!(
                        "File search with pgvector requires the 'database-postgres' feature. \
                             Rebuild with --features database-postgres or use a different vector backend."
                    );
                    return None;
                }
                config::SemanticVectorBackend::Qdrant {
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
                        tracing::error!(
                            error = %e,
                            "Failed to initialize Qdrant store for file search"
                        );
                        return None;
                    }

                    Arc::new(store)
                }
            }
        } else {
            // Priority 3: Default pgvector with "rag_chunks" table
            #[cfg(not(feature = "database-postgres"))]
            {
                tracing::warn!(
                    "File search requires a vector store backend. Configure \
                         [features.file_search.vector_backend] or rebuild with --features database-postgres."
                );
                return None;
            }

            #[cfg(feature = "database-postgres")]
            {
                let pg_pool = match db.pg_write_pool() {
                    Some(pool) => pool.clone(),
                    None => {
                        tracing::warn!(
                            "File search requires a vector store backend. Configure \
                                 [features.file_search.vector_backend] or use PostgreSQL."
                        );
                        return None;
                    }
                };

                // Use "rag_chunks" as the default table name (clear naming)
                let store = cache::vector_store::PgvectorStore::new(
                    pg_pool,
                    "rag".to_string(), // Creates "rag" for semantic + "rag_chunks" for RAG
                    embedding_config.dimensions,
                    config::PgvectorIndexType::IvfFlat,
                    config::DistanceMetric::default(), // Cosine (default)
                );

                if let Err(e) = store.initialize().await {
                    tracing::error!(
                        error = %e,
                        "Failed to initialize pgvector store for file search"
                    );
                    return None;
                }

                tracing::info!("RAG using default pgvector table 'rag_chunks'");

                Arc::new(store)
            }
        };

        // Create reranker if enabled
        let reranker: Option<Arc<dyn services::Reranker>> = if file_search_config.rerank.enabled {
            // Create a provider for the reranker using the same config as embeddings
            match Self::create_reranker_provider(
                provider_config,
                &embedding_config.provider,
                circuit_breakers,
            ) {
                Ok(provider) => {
                    let reranker = services::LlmReranker::new(
                        provider,
                        http_client.clone(),
                        file_search_config.rerank.clone(),
                        embedding_config.provider.clone(),
                    );
                    tracing::info!(
                        model = ?file_search_config.rerank.model,
                        max_results_to_rerank = file_search_config.rerank.max_results_to_rerank,
                        batch_size = file_search_config.rerank.batch_size,
                        timeout_secs = file_search_config.rerank.timeout_secs,
                        "LLM reranker enabled for file search"
                    );
                    Some(Arc::new(reranker) as Arc<dyn services::Reranker>)
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "Failed to create reranker provider, LLM re-ranking will be disabled"
                    );
                    None
                }
            }
        } else {
            None
        };

        let service = services::FileSearchService::new(
            db,
            embedding_service,
            vector_store,
            reranker,
            services::FileSearchServiceConfig {
                default_max_results: file_search_config.max_results_per_search,
                default_threshold: file_search_config.score_threshold,
                retry: file_search_config.retry.clone(),
                circuit_breaker: file_search_config.circuit_breaker.clone(),
                rerank: file_search_config.rerank.clone(),
            },
        );

        tracing::info!(
            max_results = file_search_config.max_results_per_search,
            score_threshold = file_search_config.score_threshold,
            max_iterations = file_search_config.max_iterations,
            rerank_enabled = file_search_config.rerank.enabled,
            "File search service enabled"
        );

        Some(Arc::new(service))
    }

    /// Create a provider instance for the reranker.
    ///
    /// Uses the same provider configuration as the embedding service.
    fn create_reranker_provider(
        provider_config: &config::ProviderConfig,
        provider_name: &str,
        circuit_breakers: &providers::CircuitBreakerRegistry,
    ) -> Result<Arc<dyn providers::Provider>, String> {
        match provider_config {
            config::ProviderConfig::Test(_) => {
                Err("Test provider does not support chat completions for re-ranking".into())
            }
            _ => create_provider_instance(provider_config, provider_name, circuit_breakers),
        }
    }

    /// Initialize the document processor for RAG file processing.
    ///
    /// The document processor is responsible for:
    /// - Chunking uploaded files into semantically meaningful segments
    /// - Generating embeddings for each chunk
    /// - Storing chunks in the vector store
    ///
    /// It reuses the embedding service and vector store from the file search service
    /// to ensure consistency in how documents are processed and searched.
    #[cfg(any(
        feature = "document-extraction-basic",
        feature = "document-extraction-full"
    ))]
    fn init_document_processor(
        config: &config::GatewayConfig,
        db: Option<&Arc<db::DbPool>>,
        services: Option<&services::Services>,
        file_search_service: Option<&Arc<services::FileSearchService>>,
    ) -> Option<Arc<services::DocumentProcessor>> {
        // Document processor requires database and vector stores service
        let db = db?.clone();
        let vector_stores_service = Arc::new(services?.vector_stores.clone());

        // Get embedding service and vector store from file search service (if available)
        let (embedding_service, vector_store) = file_search_service
            .map(|fs| (Some(fs.embedding_service()), Some(fs.vector_store())))
            .unwrap_or((None, None));

        // Build document processor config from file_processing config
        let processor_config: services::DocumentProcessorConfig =
            (&config.features.file_processing).into();

        // Log processing mode
        match processor_config.processing_mode {
            services::document_processor::ProcessingMode::Inline => {
                tracing::info!(
                    max_file_size_mb = processor_config.max_file_size / (1024 * 1024),
                    max_concurrent_tasks = processor_config.max_concurrent_tasks,
                    default_chunk_tokens = processor_config.default_max_chunk_tokens,
                    has_embedding_service = embedding_service.is_some(),
                    has_vector_store = vector_store.is_some(),
                    "Document processor initialized (inline mode)"
                );
            }
            services::document_processor::ProcessingMode::Queue => {
                tracing::info!(
                    max_file_size_mb = processor_config.max_file_size / (1024 * 1024),
                    has_queue_backend = processor_config.queue_backend.is_some(),
                    "Document processor initialized (queue mode)"
                );
            }
        }

        match services::DocumentProcessor::new(
            db,
            vector_stores_service,
            embedding_service,
            vector_store,
            processor_config,
        ) {
            Ok(processor) => Some(Arc::new(processor)),
            Err(e) => {
                tracing::error!(error = %e, "Failed to initialize document processor");
                None
            }
        }
    }

    /// Fetch model lists from all static (config-file) providers in parallel and
    /// store them in `self.static_models_cache`. Failures for individual providers
    /// are logged and skipped so one slow/broken provider cannot block the rest.
    pub async fn warm_static_models_cache(&self) {
        use futures::future::join_all;

        let futures: Vec<_> = self
            .config
            .providers
            .iter()
            .map(|(name, cfg)| {
                let name = name.to_owned();
                let http = self.http_client.clone();
                let cbs = self.circuit_breakers.clone();
                async move {
                    let result = providers::list_models_for_config(cfg, &name, &http, &cbs).await;
                    (name, result)
                }
            })
            .collect();

        let results = join_all(futures).await;

        let mut cache = self.static_models_cache.write().await;
        cache.retain(|name, _| self.config.providers.get(name).is_some());
        for (name, result) in results {
            match result {
                Ok(response) => {
                    cache.insert(name, response);
                }
                Err(e) => {
                    tracing::warn!(provider = %name, error = %e, "Failed to fetch models for cache warm");
                }
            }
        }
        let total_models: usize = cache.values().map(|r| r.data.len()).sum();
        tracing::info!(
            providers = cache.len(),
            models = total_models,
            "Static models cache warmed"
        );
    }
}

#[cfg(feature = "server")]
pub fn build_app(config: &config::GatewayConfig, state: AppState) -> Router {
    let mut app = Router::new()
        // Health check endpoint
        .route("/health", get(routes::health::health_check))
        .route("/health/live", get(routes::health::liveness))
        .route("/health/ready", get(routes::health::readiness));

    // OpenAPI spec and Scalar docs UI (optional)
    #[cfg(feature = "utoipa")]
    {
        app = app
            .route("/openapi.json", get(openapi_json))
            .merge(Scalar::with_url("/api/docs", openapi::ApiDoc::build()));
    }

    // Add Prometheus metrics endpoint if enabled
    if config.observability.metrics.enabled {
        let metrics_path = config
            .observability
            .metrics
            .prometheus
            .as_ref()
            .map(|p| p.path.clone())
            .unwrap_or_else(|| "/metrics".to_string());

        app = app.route(&metrics_path, get(routes::health::metrics));
    }

    app = app.nest("/api", routes::get_api_routes(state.clone()));

    // Only mount admin routes if database is configured
    if !config.database.is_none() {
        // Mount public admin routes first (no auth required)
        // These are needed for frontend bootstrap before the user logs in
        let public_admin_routes = routes::admin::get_public_admin_routes().route_layer(
            axum::middleware::from_fn_with_state(state.clone(), middleware::rate_limit_middleware),
        );
        app = app.nest("/admin", public_admin_routes);

        // Use protected routes if admin auth is configured (Idp/Iap modes), otherwise
        // unprotected (for local development with auth.mode = "none")
        if config.auth.requires_admin_auth() {
            // Apply middleware in order: admin_auth_middleware runs first,
            // then authz_middleware runs second (layers are applied in reverse order)
            // IP rate limiting runs before auth for defense in depth
            let admin_routes = routes::admin::get_protected_admin_routes()
                .route_layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    middleware::authz_middleware,
                ))
                .route_layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    middleware::admin_auth_middleware,
                ))
                .route_layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    middleware::rate_limit_middleware,
                ));
            app = app.merge(Router::new().nest("/admin", admin_routes));
        } else {
            tracing::warn!(
                "Admin routes are UNPROTECTED - configure auth.mode type = \"api_key\", \"idp\", or \"iap\" for authentication"
            );
            // Apply permissive authz middleware so handlers can still require AuthzContext
            // (fail-closed pattern) but authorization checks will always pass
            // IP rate limiting still applied for DoS protection
            let admin_routes = routes::admin::get_admin_routes()
                .route_layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    middleware::permissive_authz_middleware,
                ))
                .route_layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    middleware::rate_limit_middleware,
                ));
            app = app.merge(Router::new().nest("/admin", admin_routes));
        }
    }

    // Add auth routes
    // SSO routes are added when Session auth is configured or per-org SSO registries exist
    #[cfg(feature = "sso")]
    {
        let has_session_auth = config.auth.requires_session();
        let has_oidc_registry = state.oidc_registry.is_some();
        #[cfg(feature = "saml")]
        let has_saml = state.saml_registry.is_some();
        #[cfg(not(feature = "saml"))]
        let has_saml = false;

        // Use admin auth middleware for /auth/me when the auth mode supports
        // admin authentication (ApiKey/Idp/Iap). Only None mode leaves admin unprotected.
        // The OIDC registry is always created when a database exists (to support lazy
        // loading), so has_oidc_registry alone doesn't mean SSO is configured.
        let has_admin_auth = config.auth.requires_admin_auth();

        if has_admin_auth && (has_session_auth || has_oidc_registry || has_saml) {
            // When SSO is configured, /auth/me uses admin middleware
            let me_route =
                get(routes::auth_routes::me).route_layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    middleware::admin_auth_middleware,
                ));

            if has_session_auth || has_oidc_registry {
                // Build OIDC auth routes with IP rate limiting.
                // /me is added AFTER route_layer so it gets admin auth (from me_route)
                // but NOT rate limiting. This also avoids Axum routing conflicts between
                // nest("/auth") and route("/auth/me").
                let auth_routes = Router::new()
                    .route("/login", get(routes::auth_routes::login))
                    .route("/callback", get(routes::auth_routes::callback))
                    .route("/logout", post(routes::auth_routes::logout))
                    .route_layer(axum::middleware::from_fn_with_state(
                        state.clone(),
                        middleware::rate_limit_middleware,
                    ))
                    .route("/me", me_route);

                app = app.nest("/auth", auth_routes);
            } else {
                // SAML-only: just add /auth/me with admin middleware
                app = app.route("/auth/me", me_route);
            }

            // Add SSO discovery endpoint if database is configured (for per-org SSO)
            // This is needed for both OIDC and SAML per-org configurations.
            // Use the dedicated discover throttle (tighter than the global IP
            // rate limit) to deter SSO-domain enumeration.
            if !config.database.is_none() {
                let discover_route = get(routes::auth_routes::discover).route_layer(
                    axum::middleware::from_fn_with_state(
                        state.clone(),
                        middleware::discover_rate_limit_middleware,
                    ),
                );
                app = app.route("/auth/discover", discover_route);
            }
        } else if !config.database.is_none() {
            // When SSO feature is enabled but auth is disabled and database is available,
            // add /auth/me with permissive middleware
            let me_route =
                get(routes::auth_routes::me).route_layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    middleware::permissive_authz_middleware,
                ));
            app = app.route("/auth/me", me_route);
        }
    }

    // OAuth-style PKCE token exchange. Public endpoint (PKCE proof is the
    // authentication). Available whenever a database is configured AND the
    // flow is enabled in config; route handler also re-checks `enabled` to
    // avoid serving 500s when the flag is flipped without a restart.
    if !config.database.is_none() && config.auth.oauth_pkce.enabled {
        let oauth_token_route = post(routes::oauth_public::token).route_layer(
            axum::middleware::from_fn_with_state(state.clone(), middleware::rate_limit_middleware),
        );
        let oauth_metadata_route = get(routes::oauth_public::authorization_server_metadata)
            .route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                middleware::rate_limit_middleware,
            ));
        app = app.route("/oauth/token", oauth_token_route).route(
            // RFC 8414 Authorization Server Metadata
            "/.well-known/oauth-authorization-server",
            oauth_metadata_route,
        );
    }

    // Add SAML routes if database is configured (SAML uses per-org SSO configs from database)
    // These routes are separate from OIDC since they use HTTP-POST binding and different flows
    #[cfg(feature = "saml")]
    if !config.database.is_none() {
        let saml_routes = Router::new()
            .route("/login", get(routes::auth_routes::saml_login))
            .route("/acs", post(routes::auth_routes::saml_acs))
            .route(
                "/slo",
                get(routes::auth_routes::saml_slo).post(routes::auth_routes::saml_slo),
            )
            .route("/metadata", get(routes::auth_routes::saml_metadata))
            .route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                middleware::rate_limit_middleware,
            ));

        app = app.nest("/auth/saml", saml_routes);
        tracing::debug!("SAML 2.0 authentication routes enabled at /auth/saml/");
    }

    // Add SCIM routes for automated user provisioning from IdPs
    // SCIM requires database to be configured (for token storage and user/group mappings)
    #[cfg(feature = "sso")]
    if !config.database.is_none() {
        app = app.nest("/scim", routes::scim_routes(state.clone()));
        tracing::info!("SCIM 2.0 provisioning endpoints enabled at /scim/v2/");
    }

    // Add WebSocket route for real-time event subscriptions if enabled
    #[cfg(feature = "server")]
    if config.features.websocket.enabled {
        app = app.route("/ws/events", get(routes::ws_handler));
        tracing::info!("WebSocket event subscriptions enabled at /ws/events");
    }

    // Serve documentation site if enabled (must be before UI to avoid fallback conflicts)
    if config.docs.enabled {
        app = add_docs_routes(app, config);
    }

    // Serve static UI files if enabled
    if config.ui.enabled {
        app = add_ui_routes(app, config);
    }

    // Add request ID middleware first, then cookies layer for session management
    // Security headers are added to all responses
    app = app
        .layer(axum::middleware::from_fn(middleware::request_id_middleware))
        .layer(tower_cookies::CookieManagerLayer::new())
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            middleware::security_headers_middleware,
        ));

    // Apply CORS layer if enabled (layers are applied in reverse order, so this runs first)
    if let Some(cors_layer) = config.server.cors.clone().into_layer() {
        app = app.layer(cors_layer);
    }

    // Body limits are layered:
    //   * Per-route `DefaultBodyLimit::max(N)` (e.g. audio / files) overrides
    //     the global axum extractor default for those routes.
    //   * `DefaultBodyLimit::max(body_limit_bytes)` provides the default cap
    //     enforced by axum extractors for everything else.
    //   * `RequestBodyLimitLayer` is the hard tower-level cap, sized to the
    //     largest configured route limit so the route-level caps are not
    //     stomped on by an outer layer.
    let max_body_limit = config
        .server
        .body_limit_bytes
        .max(config.server.audio_body_limit_bytes)
        .max(config.server.files_body_limit_bytes);
    app.layer(axum::extract::DefaultBodyLimit::max(
        config.server.body_limit_bytes,
    ))
    .layer(TraceLayer::new_for_http())
    .layer(RequestBodyLimitLayer::new(max_body_limit))
    .with_state(state)
}

/// Returns the OpenAPI spec as JSON
#[cfg(feature = "utoipa")]
async fn openapi_json() -> Json<utoipa::openapi::OpenApi> {
    Json(openapi::ApiDoc::build())
}
