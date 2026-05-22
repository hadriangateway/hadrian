mod error;
#[cfg(feature = "database-postgres")]
pub mod postgres;
pub mod repos;
#[cfg(any(feature = "database-sqlite", feature = "database-wasm-sqlite"))]
pub mod sqlite;
#[cfg(feature = "database-wasm-sqlite")]
pub mod wasm_sqlite;

#[cfg(all(test, any(feature = "database-sqlite", feature = "database-postgres")))]
pub mod tests;

use std::sync::Arc;

pub use error::{DbError, DbResult};
pub use repos::*;

use crate::config::DatabaseConfig;

/// PostgreSQL pool configuration with optional read replica.
#[cfg(feature = "database-postgres")]
pub struct PgPoolPair {
    /// Primary pool for writes.
    pub write: sqlx::PgPool,
    /// Optional read replica pool. If None, reads use the write pool.
    pub read: Option<sqlx::PgPool>,
}

#[cfg(feature = "database-postgres")]
impl PgPoolPair {
    /// Get the pool to use for read operations.
    pub fn read_pool(&self) -> &sqlx::PgPool {
        self.read.as_ref().unwrap_or(&self.write)
    }

    /// Get the pool to use for write operations.
    pub fn write_pool(&self) -> &sqlx::PgPool {
        &self.write
    }
}

/// Cached repository trait objects, created once at startup.
struct CachedRepos {
    organizations: Arc<dyn OrganizationRepo>,
    projects: Arc<dyn ProjectRepo>,
    users: Arc<dyn UserRepo>,
    api_keys: Arc<dyn ApiKeyRepo>,
    providers: Arc<dyn DynamicProviderRepo>,
    usage: Arc<dyn UsageRepo>,
    model_pricing: Arc<dyn ModelPricingRepo>,
    conversations: Arc<dyn ConversationRepo>,
    audit_logs: Arc<dyn AuditLogRepo>,
    vector_stores: Arc<dyn VectorStoresRepo>,
    files: Arc<dyn FilesRepo>,
    teams: Arc<dyn TeamRepo>,
    templates: Arc<dyn TemplateRepo>,
    skills: Arc<dyn SkillRepo>,
    #[cfg(feature = "sso")]
    sso_group_mappings: Arc<dyn SsoGroupMappingRepo>,
    #[cfg(feature = "sso")]
    org_sso_configs: Arc<dyn OrgSsoConfigRepo>,
    #[cfg(feature = "sso")]
    domain_verifications: Arc<dyn DomainVerificationRepo>,
    // SCIM 2.0 provisioning
    #[cfg(feature = "sso")]
    scim_configs: Arc<dyn OrgScimConfigRepo>,
    #[cfg(feature = "sso")]
    scim_user_mappings: Arc<dyn ScimUserMappingRepo>,
    #[cfg(feature = "sso")]
    scim_group_mappings: Arc<dyn ScimGroupMappingRepo>,
    // Per-org RBAC policies
    org_rbac_policies: Arc<dyn OrgRbacPolicyRepo>,
    // Service accounts (machine identities)
    service_accounts: Arc<dyn ServiceAccountRepo>,
    // OAuth PKCE authorization codes
    oauth_authorization_codes: Arc<dyn OAuthAuthorizationCodeRepo>,
    // Persisted Responses API records
    responses: Arc<dyn ResponsesRepo>,
    // Per-response event log
    response_events: Arc<dyn ResponseEventsRepo>,
    // Containers + container_files (shell-tool /mnt/data artifacts)
    containers: Arc<dyn ContainersRepo>,
    // Parked MCP tool calls waiting on `mcp_approval_response`. Only
    // present when the `mcp` cargo feature is enabled.
    #[cfg(feature = "mcp")]
    mcp_pending_approvals: Arc<dyn McpPendingApprovalsRepo>,
}

enum PoolStorage {
    #[cfg(feature = "database-sqlite")]
    Sqlite(sqlx::SqlitePool),
    #[cfg(feature = "database-postgres")]
    Postgres(PgPoolPair),
    #[cfg(feature = "database-wasm-sqlite")]
    WasmSqlite(wasm_sqlite::WasmSqlitePool),
    #[cfg(not(any(
        feature = "database-sqlite",
        feature = "database-postgres",
        feature = "database-wasm-sqlite"
    )))]
    _None(std::convert::Infallible),
}

/// Borrowed reference to the underlying database pool.
/// Used for database-specific operations that need direct pool access.
pub enum DbPoolRef<'a> {
    #[cfg(feature = "database-sqlite")]
    Sqlite(&'a sqlx::SqlitePool),
    #[cfg(feature = "database-postgres")]
    Postgres(&'a PgPoolPair),
    #[cfg(feature = "database-wasm-sqlite")]
    WasmSqlite(&'a wasm_sqlite::WasmSqlitePool),
    #[cfg(not(any(
        feature = "database-sqlite",
        feature = "database-postgres",
        feature = "database-wasm-sqlite"
    )))]
    _None(std::convert::Infallible, std::marker::PhantomData<&'a ()>),
}

/// Database pool supporting both SQLite and PostgreSQL.
///
/// Repositories are cached at construction time to avoid allocation on each access.
pub struct DbPool {
    inner: PoolStorage,
    repos: CachedRepos,
}

impl DbPool {
    /// Create a DbPool from an existing SQLite pool.
    /// Primarily useful for testing.
    #[cfg(feature = "database-sqlite")]
    pub fn from_sqlite(pool: sqlx::SqlitePool) -> Self {
        let repos = CachedRepos {
            organizations: Arc::new(sqlite::SqliteOrganizationRepo::new(pool.clone())),
            projects: Arc::new(sqlite::SqliteProjectRepo::new(pool.clone())),
            users: Arc::new(sqlite::SqliteUserRepo::new(pool.clone())),
            api_keys: Arc::new(sqlite::SqliteApiKeyRepo::new(pool.clone())),
            providers: Arc::new(sqlite::SqliteDynamicProviderRepo::new(pool.clone())),
            usage: Arc::new(sqlite::SqliteUsageRepo::new(pool.clone())),
            model_pricing: Arc::new(sqlite::SqliteModelPricingRepo::new(pool.clone())),
            conversations: Arc::new(sqlite::SqliteConversationRepo::new(pool.clone())),
            audit_logs: Arc::new(sqlite::SqliteAuditLogRepo::new(pool.clone())),
            vector_stores: Arc::new(sqlite::SqliteVectorStoresRepo::new(pool.clone())),
            files: Arc::new(sqlite::SqliteFilesRepo::new(pool.clone())),
            teams: Arc::new(sqlite::SqliteTeamRepo::new(pool.clone())),
            templates: Arc::new(sqlite::SqliteTemplateRepo::new(pool.clone())),
            skills: Arc::new(sqlite::SqliteSkillRepo::new(pool.clone())),
            #[cfg(feature = "sso")]
            sso_group_mappings: Arc::new(sqlite::SqliteSsoGroupMappingRepo::new(pool.clone())),
            #[cfg(feature = "sso")]
            org_sso_configs: Arc::new(sqlite::SqliteOrgSsoConfigRepo::new(pool.clone())),
            #[cfg(feature = "sso")]
            domain_verifications: Arc::new(sqlite::SqliteDomainVerificationRepo::new(pool.clone())),
            #[cfg(feature = "sso")]
            scim_configs: Arc::new(sqlite::SqliteOrgScimConfigRepo::new(pool.clone())),
            #[cfg(feature = "sso")]
            scim_user_mappings: Arc::new(sqlite::SqliteScimUserMappingRepo::new(pool.clone())),
            #[cfg(feature = "sso")]
            scim_group_mappings: Arc::new(sqlite::SqliteScimGroupMappingRepo::new(pool.clone())),
            org_rbac_policies: Arc::new(sqlite::SqliteOrgRbacPolicyRepo::new(pool.clone())),
            service_accounts: Arc::new(sqlite::SqliteServiceAccountRepo::new(pool.clone())),
            oauth_authorization_codes: Arc::new(sqlite::SqliteOAuthAuthorizationCodeRepo::new(
                pool.clone(),
            )),
            responses: Arc::new(sqlite::SqliteResponsesRepo::new(pool.clone())),
            response_events: Arc::new(sqlite::SqliteResponseEventsRepo::new(pool.clone())),
            containers: Arc::new(sqlite::SqliteContainersRepo::new(pool.clone())),
            #[cfg(feature = "mcp")]
            mcp_pending_approvals: Arc::new(sqlite::SqliteMcpPendingApprovalsRepo::new(
                pool.clone(),
            )),
        };
        DbPool {
            inner: PoolStorage::Sqlite(pool),
            repos,
        }
    }

    /// Create a DbPool from a WASM SQLite pool (wa-sqlite in the browser).
    #[cfg(feature = "database-wasm-sqlite")]
    pub fn from_wasm_sqlite(pool: wasm_sqlite::WasmSqlitePool) -> Self {
        let repos = CachedRepos {
            organizations: Arc::new(sqlite::SqliteOrganizationRepo::new(pool.clone())),
            projects: Arc::new(sqlite::SqliteProjectRepo::new(pool.clone())),
            users: Arc::new(sqlite::SqliteUserRepo::new(pool.clone())),
            api_keys: Arc::new(sqlite::SqliteApiKeyRepo::new(pool.clone())),
            providers: Arc::new(sqlite::SqliteDynamicProviderRepo::new(pool.clone())),
            usage: Arc::new(sqlite::SqliteUsageRepo::new(pool.clone())),
            model_pricing: Arc::new(sqlite::SqliteModelPricingRepo::new(pool.clone())),
            conversations: Arc::new(sqlite::SqliteConversationRepo::new(pool.clone())),
            audit_logs: Arc::new(sqlite::SqliteAuditLogRepo::new(pool.clone())),
            vector_stores: Arc::new(sqlite::SqliteVectorStoresRepo::new(pool.clone())),
            files: Arc::new(sqlite::SqliteFilesRepo::new(pool.clone())),
            teams: Arc::new(sqlite::SqliteTeamRepo::new(pool.clone())),
            templates: Arc::new(sqlite::SqliteTemplateRepo::new(pool.clone())),
            skills: Arc::new(sqlite::SqliteSkillRepo::new(pool.clone())),
            #[cfg(feature = "sso")]
            sso_group_mappings: unreachable!("SSO not supported in WASM builds"),
            #[cfg(feature = "sso")]
            org_sso_configs: unreachable!("SSO not supported in WASM builds"),
            #[cfg(feature = "sso")]
            domain_verifications: unreachable!("SSO not supported in WASM builds"),
            #[cfg(feature = "sso")]
            scim_configs: unreachable!("SSO not supported in WASM builds"),
            #[cfg(feature = "sso")]
            scim_user_mappings: unreachable!("SSO not supported in WASM builds"),
            #[cfg(feature = "sso")]
            scim_group_mappings: unreachable!("SSO not supported in WASM builds"),
            org_rbac_policies: Arc::new(sqlite::SqliteOrgRbacPolicyRepo::new(pool.clone())),
            service_accounts: Arc::new(sqlite::SqliteServiceAccountRepo::new(pool.clone())),
            oauth_authorization_codes: Arc::new(sqlite::SqliteOAuthAuthorizationCodeRepo::new(
                pool.clone(),
            )),
            responses: Arc::new(sqlite::SqliteResponsesRepo::new(pool.clone())),
            response_events: Arc::new(sqlite::SqliteResponseEventsRepo::new(pool.clone())),
            containers: Arc::new(sqlite::SqliteContainersRepo::new(pool.clone())),
            #[cfg(feature = "mcp")]
            mcp_pending_approvals: Arc::new(sqlite::SqliteMcpPendingApprovalsRepo::new(
                pool.clone(),
            )),
        };
        DbPool {
            inner: PoolStorage::WasmSqlite(pool),
            repos,
        }
    }

    /// Create a DbPool from existing PostgreSQL pools.
    /// Primarily useful for testing.
    #[cfg(feature = "database-postgres")]
    pub fn from_postgres(write_pool: sqlx::PgPool, read_pool: Option<sqlx::PgPool>) -> Self {
        let repos = CachedRepos {
            organizations: Arc::new(postgres::PostgresOrganizationRepo::new(
                write_pool.clone(),
                read_pool.clone(),
            )),
            projects: Arc::new(postgres::PostgresProjectRepo::new(
                write_pool.clone(),
                read_pool.clone(),
            )),
            users: Arc::new(postgres::PostgresUserRepo::new(
                write_pool.clone(),
                read_pool.clone(),
            )),
            api_keys: Arc::new(postgres::PostgresApiKeyRepo::new(
                write_pool.clone(),
                read_pool.clone(),
            )),
            providers: Arc::new(postgres::PostgresDynamicProviderRepo::new(
                write_pool.clone(),
                read_pool.clone(),
            )),
            usage: Arc::new(postgres::PostgresUsageRepo::new(
                write_pool.clone(),
                read_pool.clone(),
            )),
            model_pricing: Arc::new(postgres::PostgresModelPricingRepo::new(
                write_pool.clone(),
                read_pool.clone(),
            )),
            conversations: Arc::new(postgres::PostgresConversationRepo::new(
                write_pool.clone(),
                read_pool.clone(),
            )),
            audit_logs: Arc::new(postgres::PostgresAuditLogRepo::new(
                write_pool.clone(),
                read_pool.clone(),
            )),
            vector_stores: Arc::new(postgres::PostgresVectorStoresRepo::new(
                write_pool.clone(),
                read_pool.clone(),
            )),
            files: Arc::new(postgres::PostgresFilesRepo::new(
                write_pool.clone(),
                read_pool.clone(),
            )),
            teams: Arc::new(postgres::PostgresTeamRepo::new(
                write_pool.clone(),
                read_pool.clone(),
            )),
            templates: Arc::new(postgres::PostgresTemplateRepo::new(
                write_pool.clone(),
                read_pool.clone(),
            )),
            skills: Arc::new(postgres::PostgresSkillRepo::new(
                write_pool.clone(),
                read_pool.clone(),
            )),
            #[cfg(feature = "sso")]
            sso_group_mappings: Arc::new(postgres::PostgresSsoGroupMappingRepo::new(
                write_pool.clone(),
                read_pool.clone(),
            )),
            #[cfg(feature = "sso")]
            org_sso_configs: Arc::new(postgres::PostgresOrgSsoConfigRepo::new(
                write_pool.clone(),
                read_pool.clone(),
            )),
            #[cfg(feature = "sso")]
            domain_verifications: Arc::new(postgres::PostgresDomainVerificationRepo::new(
                write_pool.clone(),
                read_pool.clone(),
            )),
            #[cfg(feature = "sso")]
            scim_configs: Arc::new(postgres::PostgresOrgScimConfigRepo::new(
                write_pool.clone(),
                read_pool.clone(),
            )),
            #[cfg(feature = "sso")]
            scim_user_mappings: Arc::new(postgres::PostgresScimUserMappingRepo::new(
                write_pool.clone(),
                read_pool.clone(),
            )),
            #[cfg(feature = "sso")]
            scim_group_mappings: Arc::new(postgres::PostgresScimGroupMappingRepo::new(
                write_pool.clone(),
                read_pool.clone(),
            )),
            org_rbac_policies: Arc::new(postgres::PostgresOrgRbacPolicyRepo::new(
                write_pool.clone(),
                read_pool.clone(),
            )),
            service_accounts: Arc::new(postgres::PostgresServiceAccountRepo::new(
                write_pool.clone(),
                read_pool.clone(),
            )),
            oauth_authorization_codes: Arc::new(postgres::PostgresOAuthAuthorizationCodeRepo::new(
                write_pool.clone(),
                read_pool.clone(),
            )),
            responses: Arc::new(postgres::PostgresResponsesRepo::new(
                write_pool.clone(),
                read_pool.clone(),
            )),
            response_events: Arc::new(postgres::PostgresResponseEventsRepo::new(
                write_pool.clone(),
                read_pool.clone(),
            )),
            containers: Arc::new(postgres::PostgresContainersRepo::new(
                write_pool.clone(),
                read_pool.clone(),
            )),
            #[cfg(feature = "mcp")]
            mcp_pending_approvals: Arc::new(postgres::PostgresMcpPendingApprovalsRepo::new(
                write_pool.clone(),
            )),
        };
        DbPool {
            inner: PoolStorage::Postgres(PgPoolPair {
                write: write_pool,
                read: read_pool,
            }),
            repos,
        }
    }

    /// Create a database pool from configuration
    pub async fn from_config(config: &DatabaseConfig) -> DbResult<Self> {
        match config {
            DatabaseConfig::None => Err(DbError::NotConfigured),
            #[cfg(feature = "database-sqlite")]
            DatabaseConfig::Sqlite(cfg) => {
                let pool = sqlx::sqlite::SqlitePoolOptions::new()
                    .max_connections(cfg.max_connections)
                    .connect_with(
                        sqlx::sqlite::SqliteConnectOptions::new()
                            .filename(&cfg.path)
                            .create_if_missing(cfg.create_if_missing)
                            .foreign_keys(true)
                            .journal_mode(if cfg.wal_mode {
                                sqlx::sqlite::SqliteJournalMode::Wal
                            } else {
                                sqlx::sqlite::SqliteJournalMode::Delete
                            })
                            .busy_timeout(std::time::Duration::from_millis(cfg.busy_timeout_ms)),
                    )
                    .await?;

                let fk_check: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
                    .fetch_one(&pool)
                    .await?;
                if fk_check != 1 {
                    return Err(DbError::NotConfigured);
                }

                let repos = CachedRepos {
                    organizations: Arc::new(sqlite::SqliteOrganizationRepo::new(pool.clone())),
                    projects: Arc::new(sqlite::SqliteProjectRepo::new(pool.clone())),
                    users: Arc::new(sqlite::SqliteUserRepo::new(pool.clone())),
                    api_keys: Arc::new(sqlite::SqliteApiKeyRepo::new(pool.clone())),
                    providers: Arc::new(sqlite::SqliteDynamicProviderRepo::new(pool.clone())),
                    usage: Arc::new(sqlite::SqliteUsageRepo::new(pool.clone())),
                    model_pricing: Arc::new(sqlite::SqliteModelPricingRepo::new(pool.clone())),
                    conversations: Arc::new(sqlite::SqliteConversationRepo::new(pool.clone())),
                    audit_logs: Arc::new(sqlite::SqliteAuditLogRepo::new(pool.clone())),
                    vector_stores: Arc::new(sqlite::SqliteVectorStoresRepo::new(pool.clone())),
                    files: Arc::new(sqlite::SqliteFilesRepo::new(pool.clone())),
                    teams: Arc::new(sqlite::SqliteTeamRepo::new(pool.clone())),
                    templates: Arc::new(sqlite::SqliteTemplateRepo::new(pool.clone())),
                    skills: Arc::new(sqlite::SqliteSkillRepo::new(pool.clone())),
                    #[cfg(feature = "sso")]
                    sso_group_mappings: Arc::new(sqlite::SqliteSsoGroupMappingRepo::new(
                        pool.clone(),
                    )),
                    #[cfg(feature = "sso")]
                    org_sso_configs: Arc::new(sqlite::SqliteOrgSsoConfigRepo::new(pool.clone())),
                    #[cfg(feature = "sso")]
                    domain_verifications: Arc::new(sqlite::SqliteDomainVerificationRepo::new(
                        pool.clone(),
                    )),
                    #[cfg(feature = "sso")]
                    scim_configs: Arc::new(sqlite::SqliteOrgScimConfigRepo::new(pool.clone())),
                    #[cfg(feature = "sso")]
                    scim_user_mappings: Arc::new(sqlite::SqliteScimUserMappingRepo::new(
                        pool.clone(),
                    )),
                    #[cfg(feature = "sso")]
                    scim_group_mappings: Arc::new(sqlite::SqliteScimGroupMappingRepo::new(
                        pool.clone(),
                    )),
                    org_rbac_policies: Arc::new(sqlite::SqliteOrgRbacPolicyRepo::new(pool.clone())),
                    service_accounts: Arc::new(sqlite::SqliteServiceAccountRepo::new(pool.clone())),
                    oauth_authorization_codes: Arc::new(
                        sqlite::SqliteOAuthAuthorizationCodeRepo::new(pool.clone()),
                    ),
                    responses: Arc::new(sqlite::SqliteResponsesRepo::new(pool.clone())),
                    response_events: Arc::new(sqlite::SqliteResponseEventsRepo::new(pool.clone())),
                    containers: Arc::new(sqlite::SqliteContainersRepo::new(pool.clone())),
                    #[cfg(feature = "mcp")]
                    mcp_pending_approvals: Arc::new(sqlite::SqliteMcpPendingApprovalsRepo::new(
                        pool.clone(),
                    )),
                };

                Ok(DbPool {
                    inner: PoolStorage::Sqlite(pool),
                    repos,
                })
            }
            #[cfg(feature = "database-postgres")]
            DatabaseConfig::Postgres(cfg) => {
                let ssl_mode = match cfg.ssl_mode {
                    crate::config::PostgresSslMode::Disable => sqlx::postgres::PgSslMode::Disable,
                    crate::config::PostgresSslMode::Prefer => sqlx::postgres::PgSslMode::Prefer,
                    crate::config::PostgresSslMode::Require => sqlx::postgres::PgSslMode::Require,
                    crate::config::PostgresSslMode::VerifyCa => sqlx::postgres::PgSslMode::VerifyCa,
                    crate::config::PostgresSslMode::VerifyFull => {
                        sqlx::postgres::PgSslMode::VerifyFull
                    }
                };
                let connect_opts =
                    |url: &str| -> Result<sqlx::postgres::PgConnectOptions, DbError> {
                        let opts: sqlx::postgres::PgConnectOptions = url.parse().map_err(|e| {
                            DbError::Validation(format!("Invalid Postgres URL: {e}"))
                        })?;
                        Ok(opts.ssl_mode(ssl_mode))
                    };
                let pool_opts = || {
                    sqlx::postgres::PgPoolOptions::new()
                        .min_connections(cfg.min_connections)
                        .max_connections(cfg.max_connections)
                        .acquire_timeout(std::time::Duration::from_secs(cfg.connect_timeout_secs))
                        .idle_timeout(std::time::Duration::from_secs(cfg.idle_timeout_secs))
                };
                let write_pool = pool_opts().connect_with(connect_opts(&cfg.url)?).await?;

                let read_pool = if let Some(read_url) = &cfg.read_url {
                    tracing::info!("Configuring read replica pool");
                    Some(pool_opts().connect_with(connect_opts(read_url)?).await?)
                } else {
                    None
                };

                let repos = CachedRepos {
                    organizations: Arc::new(postgres::PostgresOrganizationRepo::new(
                        write_pool.clone(),
                        read_pool.clone(),
                    )),
                    projects: Arc::new(postgres::PostgresProjectRepo::new(
                        write_pool.clone(),
                        read_pool.clone(),
                    )),
                    users: Arc::new(postgres::PostgresUserRepo::new(
                        write_pool.clone(),
                        read_pool.clone(),
                    )),
                    api_keys: Arc::new(postgres::PostgresApiKeyRepo::new(
                        write_pool.clone(),
                        read_pool.clone(),
                    )),
                    providers: Arc::new(postgres::PostgresDynamicProviderRepo::new(
                        write_pool.clone(),
                        read_pool.clone(),
                    )),
                    usage: Arc::new(postgres::PostgresUsageRepo::new(
                        write_pool.clone(),
                        read_pool.clone(),
                    )),
                    model_pricing: Arc::new(postgres::PostgresModelPricingRepo::new(
                        write_pool.clone(),
                        read_pool.clone(),
                    )),
                    conversations: Arc::new(postgres::PostgresConversationRepo::new(
                        write_pool.clone(),
                        read_pool.clone(),
                    )),
                    audit_logs: Arc::new(postgres::PostgresAuditLogRepo::new(
                        write_pool.clone(),
                        read_pool.clone(),
                    )),
                    vector_stores: Arc::new(postgres::PostgresVectorStoresRepo::new(
                        write_pool.clone(),
                        read_pool.clone(),
                    )),
                    files: Arc::new(postgres::PostgresFilesRepo::new(
                        write_pool.clone(),
                        read_pool.clone(),
                    )),
                    teams: Arc::new(postgres::PostgresTeamRepo::new(
                        write_pool.clone(),
                        read_pool.clone(),
                    )),
                    templates: Arc::new(postgres::PostgresTemplateRepo::new(
                        write_pool.clone(),
                        read_pool.clone(),
                    )),
                    skills: Arc::new(postgres::PostgresSkillRepo::new(
                        write_pool.clone(),
                        read_pool.clone(),
                    )),
                    #[cfg(feature = "sso")]
                    sso_group_mappings: Arc::new(postgres::PostgresSsoGroupMappingRepo::new(
                        write_pool.clone(),
                        read_pool.clone(),
                    )),
                    #[cfg(feature = "sso")]
                    org_sso_configs: Arc::new(postgres::PostgresOrgSsoConfigRepo::new(
                        write_pool.clone(),
                        read_pool.clone(),
                    )),
                    #[cfg(feature = "sso")]
                    domain_verifications: Arc::new(postgres::PostgresDomainVerificationRepo::new(
                        write_pool.clone(),
                        read_pool.clone(),
                    )),
                    #[cfg(feature = "sso")]
                    scim_configs: Arc::new(postgres::PostgresOrgScimConfigRepo::new(
                        write_pool.clone(),
                        read_pool.clone(),
                    )),
                    #[cfg(feature = "sso")]
                    scim_user_mappings: Arc::new(postgres::PostgresScimUserMappingRepo::new(
                        write_pool.clone(),
                        read_pool.clone(),
                    )),
                    #[cfg(feature = "sso")]
                    scim_group_mappings: Arc::new(postgres::PostgresScimGroupMappingRepo::new(
                        write_pool.clone(),
                        read_pool.clone(),
                    )),
                    org_rbac_policies: Arc::new(postgres::PostgresOrgRbacPolicyRepo::new(
                        write_pool.clone(),
                        read_pool.clone(),
                    )),
                    service_accounts: Arc::new(postgres::PostgresServiceAccountRepo::new(
                        write_pool.clone(),
                        read_pool.clone(),
                    )),
                    oauth_authorization_codes: Arc::new(
                        postgres::PostgresOAuthAuthorizationCodeRepo::new(
                            write_pool.clone(),
                            read_pool.clone(),
                        ),
                    ),
                    responses: Arc::new(postgres::PostgresResponsesRepo::new(
                        write_pool.clone(),
                        read_pool.clone(),
                    )),
                    response_events: Arc::new(postgres::PostgresResponseEventsRepo::new(
                        write_pool.clone(),
                        read_pool.clone(),
                    )),
                    containers: Arc::new(postgres::PostgresContainersRepo::new(
                        write_pool.clone(),
                        read_pool.clone(),
                    )),
                    #[cfg(feature = "mcp")]
                    mcp_pending_approvals: Arc::new(
                        postgres::PostgresMcpPendingApprovalsRepo::new(write_pool.clone()),
                    ),
                };

                Ok(DbPool {
                    inner: PoolStorage::Postgres(PgPoolPair {
                        write: write_pool,
                        read: read_pool,
                    }),
                    repos,
                })
            }
        }
    }

    /// Run database migrations using sqlx's migration runner
    /// This automatically creates and manages a _sqlx_migrations table
    /// Migrations always run on the primary (write) pool.
    pub async fn run_migrations(&self) -> DbResult<()> {
        match &self.inner {
            #[cfg(feature = "database-sqlite")]
            PoolStorage::Sqlite(pool) => {
                tracing::info!("Running SQLite migrations");
                sqlx::migrate!("./migrations_sqlx/sqlite").run(pool).await?;
                tracing::info!("SQLite migrations completed successfully");
                Ok(())
            }
            #[cfg(feature = "database-postgres")]
            PoolStorage::Postgres(pools) => {
                tracing::info!("Running PostgreSQL migrations");
                sqlx::migrate!("./migrations_sqlx/postgres")
                    .run(&pools.write)
                    .await?;
                tracing::info!("PostgreSQL migrations completed successfully");
                Ok(())
            }
            #[cfg(feature = "database-wasm-sqlite")]
            PoolStorage::WasmSqlite(pool) => {
                tracing::info!("Running WASM SQLite migrations");
                pool.run_migrations().await?;
                Ok(())
            }
            #[cfg(not(any(
                feature = "database-sqlite",
                feature = "database-postgres",
                feature = "database-wasm-sqlite"
            )))]
            PoolStorage::_None(infallible) => match *infallible {},
        }
    }

    /// Get organization repository
    pub fn organizations(&self) -> Arc<dyn OrganizationRepo> {
        Arc::clone(&self.repos.organizations)
    }

    /// Get project repository
    pub fn projects(&self) -> Arc<dyn ProjectRepo> {
        Arc::clone(&self.repos.projects)
    }

    /// Get user repository
    pub fn users(&self) -> Arc<dyn UserRepo> {
        Arc::clone(&self.repos.users)
    }

    /// Get API key repository
    pub fn api_keys(&self) -> Arc<dyn ApiKeyRepo> {
        Arc::clone(&self.repos.api_keys)
    }

    /// Get dynamic provider repository
    pub fn providers(&self) -> Arc<dyn DynamicProviderRepo> {
        Arc::clone(&self.repos.providers)
    }

    /// Get usage repository
    pub fn usage(&self) -> Arc<dyn UsageRepo> {
        Arc::clone(&self.repos.usage)
    }

    /// Get model pricing repository
    pub fn model_pricing(&self) -> Arc<dyn ModelPricingRepo> {
        Arc::clone(&self.repos.model_pricing)
    }

    /// Get conversation repository
    pub fn conversations(&self) -> Arc<dyn ConversationRepo> {
        Arc::clone(&self.repos.conversations)
    }

    /// Get audit log repository
    pub fn audit_logs(&self) -> Arc<dyn AuditLogRepo> {
        Arc::clone(&self.repos.audit_logs)
    }

    /// Get collections repository
    pub fn vector_stores(&self) -> Arc<dyn VectorStoresRepo> {
        Arc::clone(&self.repos.vector_stores)
    }

    /// Get files repository (OpenAI Files API)
    pub fn files(&self) -> Arc<dyn FilesRepo> {
        Arc::clone(&self.repos.files)
    }

    /// Get team repository
    pub fn teams(&self) -> Arc<dyn TeamRepo> {
        Arc::clone(&self.repos.teams)
    }

    /// Get template repository
    pub fn templates(&self) -> Arc<dyn TemplateRepo> {
        Arc::clone(&self.repos.templates)
    }

    /// Get skill repository
    pub fn skills(&self) -> Arc<dyn SkillRepo> {
        Arc::clone(&self.repos.skills)
    }

    /// Get SSO group mapping repository
    #[cfg(feature = "sso")]
    pub fn sso_group_mappings(&self) -> Arc<dyn SsoGroupMappingRepo> {
        Arc::clone(&self.repos.sso_group_mappings)
    }

    /// Get organization SSO config repository
    #[cfg(feature = "sso")]
    pub fn org_sso_configs(&self) -> Arc<dyn OrgSsoConfigRepo> {
        Arc::clone(&self.repos.org_sso_configs)
    }

    /// Get domain verification repository
    #[cfg(feature = "sso")]
    pub fn domain_verifications(&self) -> Arc<dyn DomainVerificationRepo> {
        Arc::clone(&self.repos.domain_verifications)
    }

    /// Get organization SCIM config repository
    #[cfg(feature = "sso")]
    pub fn scim_configs(&self) -> Arc<dyn OrgScimConfigRepo> {
        Arc::clone(&self.repos.scim_configs)
    }

    /// Get SCIM user mapping repository
    #[cfg(feature = "sso")]
    pub fn scim_user_mappings(&self) -> Arc<dyn ScimUserMappingRepo> {
        Arc::clone(&self.repos.scim_user_mappings)
    }

    /// Get SCIM group mapping repository
    #[cfg(feature = "sso")]
    pub fn scim_group_mappings(&self) -> Arc<dyn ScimGroupMappingRepo> {
        Arc::clone(&self.repos.scim_group_mappings)
    }

    /// Get organization RBAC policy repository
    pub fn org_rbac_policies(&self) -> Arc<dyn OrgRbacPolicyRepo> {
        Arc::clone(&self.repos.org_rbac_policies)
    }

    /// Get service account repository
    pub fn service_accounts(&self) -> Arc<dyn ServiceAccountRepo> {
        Arc::clone(&self.repos.service_accounts)
    }

    /// Get OAuth PKCE authorization code repository
    pub fn oauth_authorization_codes(&self) -> Arc<dyn OAuthAuthorizationCodeRepo> {
        Arc::clone(&self.repos.oauth_authorization_codes)
    }

    /// Get persisted Responses API record repository.
    pub fn responses(&self) -> Arc<dyn ResponsesRepo> {
        Arc::clone(&self.repos.responses)
    }

    /// Get the response event log repository.
    pub fn response_events(&self) -> Arc<dyn ResponseEventsRepo> {
        Arc::clone(&self.repos.response_events)
    }

    /// Get the containers + container_files repository.
    pub fn containers(&self) -> Arc<dyn ContainersRepo> {
        Arc::clone(&self.repos.containers)
    }

    /// Get the MCP pending-approvals repository. Used by the
    /// `McpExecutor` to park gated calls and by the routes layer to
    /// resume them.
    #[cfg(feature = "mcp")]
    pub fn mcp_pending_approvals(&self) -> Arc<dyn McpPendingApprovalsRepo> {
        Arc::clone(&self.repos.mcp_pending_approvals)
    }

    /// Get a reference to the underlying database pool.
    /// Useful for database-specific operations that need direct pool access.
    pub fn pool(&self) -> DbPoolRef<'_> {
        match &self.inner {
            #[cfg(feature = "database-sqlite")]
            PoolStorage::Sqlite(pool) => DbPoolRef::Sqlite(pool),
            #[cfg(feature = "database-postgres")]
            PoolStorage::Postgres(pools) => DbPoolRef::Postgres(pools),
            #[cfg(feature = "database-wasm-sqlite")]
            PoolStorage::WasmSqlite(pool) => DbPoolRef::WasmSqlite(pool),
            #[cfg(not(any(
                feature = "database-sqlite",
                feature = "database-postgres",
                feature = "database-wasm-sqlite"
            )))]
            PoolStorage::_None(infallible) => match *infallible {},
        }
    }

    /// Get the PostgreSQL write pool if using Postgres.
    /// Returns None for SQLite databases.
    #[cfg(feature = "database-postgres")]
    pub fn pg_write_pool(&self) -> Option<&sqlx::PgPool> {
        match &self.inner {
            #[cfg(feature = "database-sqlite")]
            PoolStorage::Sqlite(_) => None,
            PoolStorage::Postgres(pools) => Some(&pools.write),
        }
    }

    /// Health check for database connectivity
    pub async fn health_check(&self) -> DbResult<()> {
        match &self.inner {
            #[cfg(feature = "database-sqlite")]
            PoolStorage::Sqlite(pool) => {
                sqlx::query("SELECT 1").execute(pool).await?;
                Ok(())
            }
            #[cfg(feature = "database-postgres")]
            PoolStorage::Postgres(pools) => {
                // Check both write and read pools
                sqlx::query("SELECT 1").execute(&pools.write).await?;
                if let Some(read) = &pools.read {
                    sqlx::query("SELECT 1").execute(read).await?;
                }
                Ok(())
            }
            #[cfg(feature = "database-wasm-sqlite")]
            PoolStorage::WasmSqlite(pool) => {
                pool.execute_query("SELECT 1", &[]).await?;
                Ok(())
            }
            #[cfg(not(any(
                feature = "database-sqlite",
                feature = "database-postgres",
                feature = "database-wasm-sqlite"
            )))]
            PoolStorage::_None(infallible) => match *infallible {},
        }
    }
}
