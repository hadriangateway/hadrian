//! Interactive configuration wizard for generating hadrian.toml files.
//!
//! The wizard guides users through configuring:
//! - Deployment mode (local, single-node, multi-node)
//! - Database (none, SQLite, PostgreSQL)
//! - Cache (none, memory, Redis)
//! - Providers (OpenAI, Anthropic, Bedrock, Vertex, Azure OpenAI, custom)
//! - Authentication (none, API key, OIDC)
//! - Budget and rate limits

use std::path::PathBuf;

use dialoguer::{Confirm, Input, Password, Select, theme::ColorfulTheme};
use rust_decimal::Decimal;

/// Wizard errors.
#[derive(Debug, thiserror::Error)]
pub enum WizardError {
    #[error("User cancelled the wizard")]
    Cancelled,
    #[error("Dialoguer error: {0}")]
    Dialoguer(#[from] dialoguer::Error),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Result of running the configuration wizard.
#[derive(Debug)]
pub struct WizardResult {
    /// Generated TOML configuration content.
    pub config: String,
    /// Suggested output path.
    pub path: PathBuf,
}

/// Deployment mode selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeploymentMode {
    /// Local development - minimal config, SQLite + memory cache
    LocalDev,
    /// Single-node production - SQLite + Redis
    SingleNode,
    /// Multi-node production - PostgreSQL + Redis
    MultiNode,
    /// Custom - manual configuration of each component
    Custom,
}

impl std::fmt::Display for DeploymentMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LocalDev => write!(f, "Local development (SQLite + memory cache, no auth)"),
            Self::SingleNode => write!(f, "Single-node production (SQLite + Redis, API key auth)"),
            Self::MultiNode => write!(f, "Multi-node production (PostgreSQL + Redis, OIDC)"),
            Self::Custom => write!(f, "Custom configuration"),
        }
    }
}

/// Database type selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DatabaseType {
    None,
    Sqlite,
    Postgres,
}

impl std::fmt::Display for DatabaseType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::None => write!(f, "None (stateless mode, local dev only)"),
            Self::Sqlite => write!(f, "SQLite (single-node, persistent)"),
            Self::Postgres => write!(f, "PostgreSQL (multi-node, production)"),
        }
    }
}

/// Cache type selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CacheType {
    None,
    Memory,
    Redis,
}

impl std::fmt::Display for CacheType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::None => write!(f, "None (no rate limiting or budget enforcement)"),
            Self::Memory => write!(f, "Memory (single-node, lost on restart)"),
            Self::Redis => write!(f, "Redis (multi-node, persistent)"),
        }
    }
}

/// Provider type selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProviderType {
    OpenAi,
    Anthropic,
    Bedrock,
    Vertex,
    Gemini,
    AzureOpenAi,
    OpenRouter,
    Ollama,
}

impl std::fmt::Display for ProviderType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OpenAi => write!(f, "OpenAI"),
            Self::Anthropic => write!(f, "Anthropic (Claude)"),
            Self::Bedrock => write!(f, "AWS Bedrock"),
            Self::Vertex => write!(f, "Google Vertex AI"),
            Self::Gemini => write!(f, "Google Gemini (API key)"),
            Self::AzureOpenAi => write!(f, "Azure OpenAI"),
            Self::OpenRouter => write!(f, "OpenRouter (200+ models)"),
            Self::Ollama => write!(f, "Ollama (local models)"),
        }
    }
}

impl ProviderType {
    fn config_type(&self) -> &'static str {
        match self {
            Self::OpenAi | Self::OpenRouter | Self::Ollama => "open_ai",
            Self::Anthropic => "anthropic",
            Self::Bedrock => "bedrock",
            Self::Vertex => "vertex",
            Self::Gemini => "gemini",
            Self::AzureOpenAi => "azure_open_ai",
        }
    }

    fn default_name(&self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
            Self::Bedrock => "bedrock",
            Self::Vertex => "vertex",
            Self::Gemini => "gemini",
            Self::AzureOpenAi => "azure",
            Self::OpenRouter => "openrouter",
            Self::Ollama => "ollama",
        }
    }

    fn needs_api_key(&self) -> bool {
        !matches!(self, Self::Bedrock | Self::Vertex | Self::Ollama)
    }

    fn env_var_name(&self) -> &'static str {
        match self {
            Self::OpenAi => "OPENAI_API_KEY",
            Self::Anthropic => "ANTHROPIC_API_KEY",
            Self::Bedrock => "",
            Self::Vertex => "",
            Self::Gemini => "GEMINI_API_KEY",
            Self::AzureOpenAi => "AZURE_OPENAI_API_KEY",
            Self::OpenRouter => "OPENROUTER_API_KEY",
            Self::Ollama => "",
        }
    }

    fn default_base_url(&self) -> Option<&'static str> {
        match self {
            Self::OpenRouter => Some("https://openrouter.ai/api/v1/"),
            Self::Ollama => Some("http://localhost:11434/v1"),
            _ => None,
        }
    }
}

/// Provider configuration collected from the wizard.
#[derive(Debug)]
struct ProviderConfig {
    provider_type: ProviderType,
    name: String,
    api_key: Option<String>,
    base_url: Option<String>,
    region: Option<String>,
    project_id: Option<String>,
}

/// API authentication type selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApiAuthType {
    None,
    ApiKey,
    Oidc,
}

impl std::fmt::Display for ApiAuthType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::None => write!(f, "None (no authentication, local dev only)"),
            Self::ApiKey => write!(f, "API Key (database-backed keys)"),
            Self::Oidc => write!(f, "OIDC (OAuth/OpenID Connect)"),
        }
    }
}

/// Authentication mode configuration.
#[derive(Debug)]
enum AuthModeConfig {
    None,
    ApiKey { key_prefix: String },
    Idp(OidcConfig),
}

/// OIDC configuration.
#[derive(Debug)]
struct OidcConfig {
    issuer: String,
    client_id: String,
    client_secret: String,
    redirect_uri: String,
}

/// Rate limiting configuration.
#[derive(Debug, Default)]
struct RateLimitConfig {
    requests_per_minute: Option<u32>,
    tokens_per_minute: Option<u32>,
    concurrent_requests: Option<u32>,
}

/// Budget configuration.
#[derive(Debug, Default)]
struct BudgetConfig {
    monthly_budget_usd: Option<Decimal>,
    daily_budget_usd: Option<Decimal>,
}

/// Complete wizard configuration collected from all steps.
#[derive(Debug)]
struct WizardConfig {
    database: DatabaseConfig,
    cache: CacheConfig,
    providers: Vec<ProviderConfig>,
    auth: AuthModeConfig,
    rate_limits: RateLimitConfig,
    budget: BudgetConfig,
}

/// Run the interactive configuration wizard.
pub fn run() -> Result<WizardResult, WizardError> {
    let theme = ColorfulTheme::default();

    println!();
    println!("╔════════════════════════════════════════════════════════════════════╗");
    println!("║           Hadrian AI Gateway - Configuration Wizard                ║");
    println!("╚════════════════════════════════════════════════════════════════════╝");
    println!();

    // Step 1: Deployment mode
    let mode = select_deployment_mode(&theme)?;

    // Based on deployment mode, determine defaults or ask for custom config
    let config = match mode {
        DeploymentMode::LocalDev => configure_local_dev(&theme)?,
        DeploymentMode::SingleNode => configure_single_node(&theme)?,
        DeploymentMode::MultiNode => configure_multi_node(&theme)?,
        DeploymentMode::Custom => configure_custom(&theme)?,
    };

    // Generate the TOML configuration
    let toml = generate_config(mode, &config);

    // Determine output path
    let default_path =
        crate::default_config_path().unwrap_or_else(|| PathBuf::from("hadrian.toml"));

    let path: String = Input::with_theme(&theme)
        .with_prompt("Output path")
        .default(default_path.to_string_lossy().to_string())
        .interact_text()?;

    // Offer to validate connections
    println!();
    if Confirm::with_theme(&theme)
        .with_prompt("Would you like to validate the configuration?")
        .default(true)
        .interact()?
    {
        validate_config(&config)?;
    }

    Ok(WizardResult {
        config: toml,
        path: PathBuf::from(path),
    })
}

fn select_deployment_mode(theme: &ColorfulTheme) -> Result<DeploymentMode, WizardError> {
    let modes = [
        DeploymentMode::LocalDev,
        DeploymentMode::SingleNode,
        DeploymentMode::MultiNode,
        DeploymentMode::Custom,
    ];

    let selection = Select::with_theme(theme)
        .with_prompt("Select deployment mode")
        .items(modes)
        .default(0)
        .interact_opt()?
        .ok_or(WizardError::Cancelled)?;

    Ok(modes[selection])
}

fn configure_local_dev(theme: &ColorfulTheme) -> Result<WizardConfig, WizardError> {
    println!();
    println!("Local development mode:");
    println!("  - SQLite database for persistent storage");
    println!("  - In-memory cache (rate limiting works, lost on restart)");
    println!("  - No authentication required");
    println!();

    let providers = configure_providers(theme)?;

    let db_path = crate::default_data_dir()
        .map(|p| p.join("hadrian.db"))
        .unwrap_or_else(|| PathBuf::from("hadrian.db"));

    Ok(WizardConfig {
        database: DatabaseConfig::Sqlite {
            path: db_path.to_string_lossy().to_string(),
        },
        cache: CacheConfig::Memory,
        providers,
        auth: AuthModeConfig::None,
        rate_limits: RateLimitConfig::default(),
        budget: BudgetConfig::default(),
    })
}

fn configure_single_node(theme: &ColorfulTheme) -> Result<WizardConfig, WizardError> {
    println!();
    println!("Single-node production mode:");
    println!("  - SQLite database for persistent storage");
    println!("  - Redis cache for rate limiting (survives restarts)");
    println!("  - API key authentication");
    println!("  - Optional rate limits and budget");
    println!();

    let providers = configure_providers(theme)?;

    let db_path = crate::default_data_dir()
        .map(|p| p.join("hadrian.db"))
        .unwrap_or_else(|| PathBuf::from("hadrian.db"));

    let redis_url: String = Input::with_theme(theme)
        .with_prompt("Redis URL")
        .default("redis://localhost:6379".to_string())
        .interact_text()?;

    // API key auth with default prefix
    let key_prefix: String = Input::with_theme(theme)
        .with_prompt("API key prefix")
        .default("gw_".to_string())
        .interact_text()?;

    // Rate limits
    let rate_limits = configure_rate_limits(theme)?;

    // Budget
    let budget = configure_budget(theme)?;

    Ok(WizardConfig {
        database: DatabaseConfig::Sqlite {
            path: db_path.to_string_lossy().to_string(),
        },
        cache: CacheConfig::Redis { url: redis_url },
        providers,
        auth: AuthModeConfig::ApiKey { key_prefix },
        rate_limits,
        budget,
    })
}

fn configure_multi_node(theme: &ColorfulTheme) -> Result<WizardConfig, WizardError> {
    println!();
    println!("Multi-node production mode:");
    println!("  - PostgreSQL database (required for multi-node)");
    println!("  - Redis cache (required for distributed rate limiting)");
    println!("  - OIDC authentication");
    println!("  - Optional rate limits and budget");
    println!();

    let providers = configure_providers(theme)?;

    let postgres_url: String = Input::with_theme(theme)
        .with_prompt("PostgreSQL URL")
        .default("postgres://user:password@localhost:5432/gateway".to_string())
        .interact_text()?;

    let redis_url: String = Input::with_theme(theme)
        .with_prompt("Redis URL")
        .default("redis://localhost:6379".to_string())
        .interact_text()?;

    // OIDC auth
    let oidc = configure_oidc(theme)?;

    // Rate limits
    let rate_limits = configure_rate_limits(theme)?;

    // Budget
    let budget = configure_budget(theme)?;

    Ok(WizardConfig {
        database: DatabaseConfig::Postgres { url: postgres_url },
        cache: CacheConfig::Redis { url: redis_url },
        providers,
        auth: AuthModeConfig::Idp(oidc),
        rate_limits,
        budget,
    })
}

fn configure_custom(theme: &ColorfulTheme) -> Result<WizardConfig, WizardError> {
    println!();
    println!("Custom configuration - configure each component individually.");
    println!();

    // Database
    let database = select_database(theme)?;

    // Cache
    let cache = select_cache(theme)?;

    // Providers
    let providers = configure_providers(theme)?;

    // Authentication
    let auth = select_auth(theme)?;

    // Rate limits
    let rate_limits = configure_rate_limits(theme)?;

    // Budget
    let budget = configure_budget(theme)?;

    Ok(WizardConfig {
        database,
        cache,
        providers,
        auth,
        rate_limits,
        budget,
    })
}

fn select_database(theme: &ColorfulTheme) -> Result<DatabaseConfig, WizardError> {
    let types = [
        DatabaseType::None,
        DatabaseType::Sqlite,
        DatabaseType::Postgres,
    ];

    let selection = Select::with_theme(theme)
        .with_prompt("Select database type")
        .items(types)
        .default(1)
        .interact_opt()?
        .ok_or(WizardError::Cancelled)?;

    match types[selection] {
        DatabaseType::None => Ok(DatabaseConfig::None),
        DatabaseType::Sqlite => {
            let default_path = crate::default_data_dir()
                .map(|p| p.join("hadrian.db"))
                .unwrap_or_else(|| PathBuf::from("hadrian.db"));

            let path: String = Input::with_theme(theme)
                .with_prompt("SQLite database path")
                .default(default_path.to_string_lossy().to_string())
                .interact_text()?;

            Ok(DatabaseConfig::Sqlite { path })
        }
        DatabaseType::Postgres => {
            let url: String = Input::with_theme(theme)
                .with_prompt("PostgreSQL URL")
                .default("postgres://user:password@localhost:5432/gateway".to_string())
                .interact_text()?;

            Ok(DatabaseConfig::Postgres { url })
        }
    }
}

fn select_cache(theme: &ColorfulTheme) -> Result<CacheConfig, WizardError> {
    let types = [CacheType::None, CacheType::Memory, CacheType::Redis];

    let selection = Select::with_theme(theme)
        .with_prompt("Select cache type")
        .items(types)
        .default(1)
        .interact_opt()?
        .ok_or(WizardError::Cancelled)?;

    match types[selection] {
        CacheType::None => Ok(CacheConfig::None),
        CacheType::Memory => Ok(CacheConfig::Memory),
        CacheType::Redis => {
            let url: String = Input::with_theme(theme)
                .with_prompt("Redis URL")
                .default("redis://localhost:6379".to_string())
                .interact_text()?;

            Ok(CacheConfig::Redis { url })
        }
    }
}

fn select_auth(theme: &ColorfulTheme) -> Result<AuthModeConfig, WizardError> {
    let types = [ApiAuthType::None, ApiAuthType::ApiKey, ApiAuthType::Oidc];

    let selection = Select::with_theme(theme)
        .with_prompt("Select authentication method")
        .items(types)
        .default(1)
        .interact_opt()?
        .ok_or(WizardError::Cancelled)?;

    match types[selection] {
        ApiAuthType::None => Ok(AuthModeConfig::None),
        ApiAuthType::ApiKey => {
            let key_prefix: String = Input::with_theme(theme)
                .with_prompt("API key prefix (e.g., 'gw_')")
                .default("gw_".to_string())
                .interact_text()?;

            Ok(AuthModeConfig::ApiKey { key_prefix })
        }
        ApiAuthType::Oidc => {
            let oidc = configure_oidc(theme)?;
            Ok(AuthModeConfig::Idp(oidc))
        }
    }
}

fn configure_oidc(theme: &ColorfulTheme) -> Result<OidcConfig, WizardError> {
    println!();
    println!("Configure OIDC authentication:");
    println!("  - You'll need to register this application with your identity provider");
    println!("  - Common providers: Keycloak, Auth0, Okta, Google, Azure AD");
    println!();

    let issuer: String = Input::with_theme(theme)
        .with_prompt("OIDC issuer URL (e.g., https://auth.example.com/realms/main)")
        .interact_text()?;

    let client_id: String = Input::with_theme(theme)
        .with_prompt("Client ID")
        .interact_text()?;

    let use_env = Confirm::with_theme(theme)
        .with_prompt("Use environment variable ${OIDC_CLIENT_SECRET}?")
        .default(true)
        .interact()?;

    let client_secret = if use_env {
        "${OIDC_CLIENT_SECRET}".to_string()
    } else {
        Password::with_theme(theme)
            .with_prompt("Client secret")
            .interact()?
    };

    let redirect_uri: String = Input::with_theme(theme)
        .with_prompt("Redirect URI")
        .default("http://localhost:8080/auth/callback".to_string())
        .interact_text()?;

    Ok(OidcConfig {
        issuer,
        client_id,
        client_secret,
        redirect_uri,
    })
}

fn configure_rate_limits(theme: &ColorfulTheme) -> Result<RateLimitConfig, WizardError> {
    let configure = Confirm::with_theme(theme)
        .with_prompt("Configure rate limits?")
        .default(false)
        .interact()?;

    if !configure {
        return Ok(RateLimitConfig::default());
    }

    println!();
    println!("Rate limits (leave blank for unlimited):");
    println!();

    let rpm_str: String = Input::with_theme(theme)
        .with_prompt("Requests per minute per user")
        .default("60".to_string())
        .allow_empty(true)
        .interact_text()?;

    let requests_per_minute = if rpm_str.is_empty() {
        None
    } else {
        rpm_str.parse().ok()
    };

    let tpm_str: String = Input::with_theme(theme)
        .with_prompt("Tokens per minute per user")
        .default("100000".to_string())
        .allow_empty(true)
        .interact_text()?;

    let tokens_per_minute = if tpm_str.is_empty() {
        None
    } else {
        tpm_str.parse().ok()
    };

    let concurrent_str: String = Input::with_theme(theme)
        .with_prompt("Max concurrent requests per user")
        .default("10".to_string())
        .allow_empty(true)
        .interact_text()?;

    let concurrent_requests = if concurrent_str.is_empty() {
        None
    } else {
        concurrent_str.parse().ok()
    };

    Ok(RateLimitConfig {
        requests_per_minute,
        tokens_per_minute,
        concurrent_requests,
    })
}

fn configure_budget(theme: &ColorfulTheme) -> Result<BudgetConfig, WizardError> {
    let configure = Confirm::with_theme(theme)
        .with_prompt("Configure budget limits?")
        .default(false)
        .interact()?;

    if !configure {
        return Ok(BudgetConfig::default());
    }

    println!();
    println!("Budget limits in USD (leave blank for unlimited):");
    println!();

    let monthly_str: String = Input::with_theme(theme)
        .with_prompt("Default monthly budget per user (USD)")
        .default("".to_string())
        .allow_empty(true)
        .interact_text()?;

    let monthly_budget_usd = if monthly_str.is_empty() {
        None
    } else {
        monthly_str.parse().ok()
    };

    let daily_str: String = Input::with_theme(theme)
        .with_prompt("Default daily budget per user (USD)")
        .default("".to_string())
        .allow_empty(true)
        .interact_text()?;

    let daily_budget_usd = if daily_str.is_empty() {
        None
    } else {
        daily_str.parse().ok()
    };

    Ok(BudgetConfig {
        monthly_budget_usd,
        daily_budget_usd,
    })
}

fn validate_config(config: &WizardConfig) -> Result<(), WizardError> {
    println!();
    println!("Validating configuration...");
    println!();

    let mut errors = Vec::new();

    // Validate database connection
    match &config.database {
        DatabaseConfig::None => {
            println!("  ✓ Database: None (stateless mode)");
        }
        DatabaseConfig::Sqlite { path } => {
            let path = PathBuf::from(path);
            if let Some(parent) = path.parent() {
                if parent.as_os_str().is_empty() || parent.exists() {
                    println!("  ✓ SQLite: Path is valid ({})", path.display());
                } else {
                    let msg = format!(
                        "SQLite: Parent directory does not exist ({})",
                        parent.display()
                    );
                    println!("  ✗ {}", msg);
                    errors.push(msg);
                }
            } else {
                println!("  ✓ SQLite: Path is valid ({})", path.display());
            }
        }
        DatabaseConfig::Postgres { url } => {
            // Basic URL format validation
            if url.starts_with("postgres://") || url.starts_with("postgresql://") {
                println!("  ✓ PostgreSQL: URL format is valid");
                println!("    Note: Run the gateway to test actual connection");
            } else {
                let msg =
                    "PostgreSQL: URL must start with postgres:// or postgresql://".to_string();
                println!("  ✗ {}", msg);
                errors.push(msg);
            }
        }
    }

    // Validate cache configuration
    match &config.cache {
        CacheConfig::None => {
            println!("  ✓ Cache: None");
        }
        CacheConfig::Memory => {
            println!("  ✓ Cache: Memory");
        }
        CacheConfig::Redis { url } => {
            if url.starts_with("redis://") || url.starts_with("rediss://") {
                println!("  ✓ Redis: URL format is valid");
                println!("    Note: Run the gateway to test actual connection");
            } else {
                let msg = "Redis: URL must start with redis:// or rediss://".to_string();
                println!("  ✗ {}", msg);
                errors.push(msg);
            }
        }
    }

    // Validate providers
    if config.providers.is_empty() {
        let msg = "No providers configured".to_string();
        println!("  ✗ {}", msg);
        errors.push(msg);
    } else {
        for provider in &config.providers {
            if provider.provider_type.needs_api_key() && provider.api_key.is_none() {
                let msg = format!("Provider '{}' requires an API key", provider.name);
                println!("  ✗ {}", msg);
                errors.push(msg);
            } else {
                println!("  ✓ Provider '{}' configured", provider.name);
            }
        }
    }

    // Validate auth
    match &config.auth {
        AuthModeConfig::None => {
            println!("  ✓ Authentication: None (local dev only)");
        }
        AuthModeConfig::ApiKey { key_prefix } => {
            if key_prefix.is_empty() {
                let msg = "API key prefix cannot be empty".to_string();
                println!("  ✗ {}", msg);
                errors.push(msg);
            } else {
                println!("  ✓ Authentication: API key (prefix: {})", key_prefix);
            }
        }
        AuthModeConfig::Idp(oidc) => {
            if oidc.issuer.is_empty() {
                let msg = "OIDC issuer URL cannot be empty".to_string();
                println!("  ✗ {}", msg);
                errors.push(msg);
            } else if oidc.client_id.is_empty() {
                let msg = "OIDC client ID cannot be empty".to_string();
                println!("  ✗ {}", msg);
                errors.push(msg);
            } else {
                println!("  ✓ Authentication: IdP (issuer: {})", oidc.issuer);
            }
        }
    }

    println!();

    if errors.is_empty() {
        println!("Configuration is valid!");
    } else {
        println!(
            "Configuration has {} error(s). You may want to fix these before running.",
            errors.len()
        );
    }

    Ok(())
}

fn configure_providers(theme: &ColorfulTheme) -> Result<Vec<ProviderConfig>, WizardError> {
    println!();
    println!("Configure LLM providers. You need at least one provider.");
    println!();

    let mut providers = Vec::new();

    loop {
        let provider = configure_single_provider(theme)?;
        providers.push(provider);

        let add_more = Confirm::with_theme(theme)
            .with_prompt("Add another provider?")
            .default(false)
            .interact()?;

        if !add_more {
            break;
        }
    }

    Ok(providers)
}

fn configure_single_provider(theme: &ColorfulTheme) -> Result<ProviderConfig, WizardError> {
    let types = [
        ProviderType::OpenAi,
        ProviderType::Anthropic,
        ProviderType::OpenRouter,
        ProviderType::Bedrock,
        ProviderType::Vertex,
        ProviderType::Gemini,
        ProviderType::AzureOpenAi,
        ProviderType::Ollama,
    ];

    let selection = Select::with_theme(theme)
        .with_prompt("Select provider type")
        .items(types)
        .default(0)
        .interact_opt()?
        .ok_or(WizardError::Cancelled)?;

    let provider_type = types[selection];

    // Provider name
    let name: String = Input::with_theme(theme)
        .with_prompt("Provider name (used in API requests)")
        .default(provider_type.default_name().to_string())
        .interact_text()?;

    // API key (if needed)
    let api_key = if provider_type.needs_api_key() {
        let use_env = Confirm::with_theme(theme)
            .with_prompt(format!(
                "Use environment variable ${{{}}}?",
                provider_type.env_var_name()
            ))
            .default(true)
            .interact()?;

        if use_env {
            Some(format!("${{{}}}", provider_type.env_var_name()))
        } else {
            let key = Password::with_theme(theme)
                .with_prompt("API key")
                .interact()?;
            Some(key)
        }
    } else {
        None
    };

    // Base URL (for providers that need it)
    let base_url = match provider_type {
        ProviderType::OpenRouter | ProviderType::Ollama => {
            let default = provider_type.default_base_url().unwrap_or("");
            let url: String = Input::with_theme(theme)
                .with_prompt("Base URL")
                .default(default.to_string())
                .interact_text()?;
            Some(url)
        }
        ProviderType::OpenAi => {
            let custom = Confirm::with_theme(theme)
                .with_prompt("Use custom base URL? (for OpenAI-compatible endpoints)")
                .default(false)
                .interact()?;

            if custom {
                let url: String = Input::with_theme(theme)
                    .with_prompt("Base URL")
                    .interact_text()?;
                Some(url)
            } else {
                None
            }
        }
        _ => None,
    };

    // AWS region (for Bedrock)
    let region = if matches!(provider_type, ProviderType::Bedrock) {
        let region: String = Input::with_theme(theme)
            .with_prompt("AWS region")
            .default("us-east-1".to_string())
            .interact_text()?;
        Some(region)
    } else {
        None
    };

    // GCP project ID (for Vertex)
    let project_id = if matches!(provider_type, ProviderType::Vertex) {
        let project: String = Input::with_theme(theme)
            .with_prompt("GCP project ID")
            .interact_text()?;
        Some(project)
    } else {
        None
    };

    Ok(ProviderConfig {
        provider_type,
        name,
        api_key,
        base_url,
        region,
        project_id,
    })
}

/// Internal representation of database config for generation.
#[derive(Debug)]
enum DatabaseConfig {
    None,
    Sqlite { path: String },
    Postgres { url: String },
}

/// Internal representation of cache config for generation.
#[derive(Debug)]
enum CacheConfig {
    None,
    Memory,
    Redis { url: String },
}

fn generate_config(mode: DeploymentMode, wizard_config: &WizardConfig) -> String {
    let mut config = String::new();

    // Header
    config.push_str("# Hadrian AI Gateway Configuration\n");
    config.push_str(&format!(
        "# Generated by wizard - {} mode\n",
        match mode {
            DeploymentMode::LocalDev => "local development",
            DeploymentMode::SingleNode => "single-node production",
            DeploymentMode::MultiNode => "multi-node production",
            DeploymentMode::Custom => "custom",
        }
    ));
    config.push('\n');

    // Server section
    config.push_str("[server]\n");
    config.push_str("host = \"0.0.0.0\"\n");
    config.push_str("port = 8080\n");
    if matches!(mode, DeploymentMode::LocalDev) {
        config.push_str("# Allow providers on localhost (e.g. Ollama)\n");
        config.push_str("allow_loopback_urls = true\n");
    }
    config.push('\n');

    // CORS (always enabled for UI)
    config.push_str("[server.cors]\n");
    config.push_str("enabled = true\n");
    config.push_str("allowed_origins = [\"http://localhost:8080\", \"http://127.0.0.1:8080\"]\n");
    config.push_str("allow_credentials = true\n");
    config.push('\n');

    // Database section
    match &wizard_config.database {
        DatabaseConfig::None => {
            config.push_str("[database]\n");
            config.push_str("type = \"none\"\n");
        }
        DatabaseConfig::Sqlite { path } => {
            config.push_str("[database]\n");
            config.push_str("type = \"sqlite\"\n");
            config.push_str(&format!("path = \"{}\"\n", escape_toml_string(path)));
        }
        DatabaseConfig::Postgres { url } => {
            config.push_str("[database]\n");
            config.push_str("type = \"postgres\"\n");
            config.push_str(&format!("url = \"{}\"\n", escape_toml_string(url)));
        }
    }
    config.push('\n');

    // Cache section
    match &wizard_config.cache {
        CacheConfig::None => {
            config.push_str("[cache]\n");
            config.push_str("type = \"none\"\n");
        }
        CacheConfig::Memory => {
            config.push_str("[cache]\n");
            config.push_str("type = \"memory\"\n");
        }
        CacheConfig::Redis { url } => {
            config.push_str("[cache]\n");
            config.push_str("type = \"redis\"\n");
            config.push_str(&format!("url = \"{}\"\n", escape_toml_string(url)));
        }
    }
    config.push('\n');

    // UI section
    config.push_str("[ui]\n");
    config.push_str("enabled = true\n");
    config.push('\n');

    // Authentication section
    match &wizard_config.auth {
        AuthModeConfig::None => {
            config.push_str("[auth.mode]\n");
            config.push_str("type = \"none\"\n");
            config.push('\n');
        }
        AuthModeConfig::ApiKey { key_prefix } => {
            config.push_str("[auth.mode]\n");
            config.push_str("type = \"api_key\"\n");
            config.push('\n');
            config.push_str("[auth.api_key]\n");
            config.push_str(&format!(
                "key_prefix = \"{}\"\n",
                escape_toml_string(key_prefix)
            ));
            config.push('\n');
        }
        AuthModeConfig::Idp(oidc) => {
            config.push_str("[auth.mode]\n");
            config.push_str("type = \"idp\"\n");
            config.push('\n');
            config.push_str("# Note: Per-org SSO is configured via the admin API.\n");
            config.push_str("# The OIDC settings below are for reference.\n");
            config.push_str(&format!(
                "# issuer = \"{}\"\n",
                escape_toml_string(&oidc.issuer)
            ));
            config.push_str(&format!(
                "# client_id = \"{}\"\n",
                escape_toml_string(&oidc.client_id)
            ));
            config.push_str(&format!(
                "# client_secret = \"{}\"\n",
                escape_toml_string(&oidc.client_secret)
            ));
            config.push_str(&format!(
                "# redirect_uri = \"{}\"\n",
                escape_toml_string(&oidc.redirect_uri)
            ));
            config.push('\n');
            config.push_str("[auth.session]\n");
            config.push_str("# Sessions are signed with this 256-bit secret. Override via the\n");
            config.push_str("# SESSION_SECRET env var in multi-replica setups so every node\n");
            config.push_str("# accepts the others' cookies.\n");
            config.push_str(&format!("secret = \"{}\"\n", generate_session_secret()));
            config.push('\n');
        }
    }

    // Limits section (only if configured)
    let has_rate_limits = wizard_config.rate_limits.requests_per_minute.is_some()
        || wizard_config.rate_limits.tokens_per_minute.is_some()
        || wizard_config.rate_limits.concurrent_requests.is_some();
    let has_budget = wizard_config.budget.monthly_budget_usd.is_some()
        || wizard_config.budget.daily_budget_usd.is_some();

    if has_rate_limits {
        config.push_str("[limits.rate_limits]\n");
        if let Some(rpm) = wizard_config.rate_limits.requests_per_minute {
            config.push_str(&format!("requests_per_minute = {}\n", rpm));
        }
        if let Some(tpm) = wizard_config.rate_limits.tokens_per_minute {
            config.push_str(&format!("tokens_per_minute = {}\n", tpm));
        }
        if let Some(concurrent) = wizard_config.rate_limits.concurrent_requests {
            config.push_str(&format!("concurrent_requests = {}\n", concurrent));
        }
        config.push('\n');
    }

    if has_budget {
        config.push_str("[limits.budgets]\n");
        if let Some(monthly) = &wizard_config.budget.monthly_budget_usd {
            config.push_str(&format!("monthly_budget_usd = \"{}\"\n", monthly));
        }
        if let Some(daily) = &wizard_config.budget.daily_budget_usd {
            config.push_str(&format!("daily_budget_usd = \"{}\"\n", daily));
        }
        config.push('\n');
    }

    // Providers section
    if let Some(first) = wizard_config.providers.first() {
        config.push_str("[providers]\n");
        config.push_str(&format!("default_provider = \"{}\"\n", first.name));
        config.push('\n');

        for provider in &wizard_config.providers {
            config.push_str(&format!("[providers.{}]\n", provider.name));
            config.push_str(&format!(
                "type = \"{}\"\n",
                provider.provider_type.config_type()
            ));

            if let Some(api_key) = &provider.api_key {
                config.push_str(&format!("api_key = \"{}\"\n", escape_toml_string(api_key)));
            }

            if let Some(base_url) = &provider.base_url {
                config.push_str(&format!(
                    "base_url = \"{}\"\n",
                    escape_toml_string(base_url)
                ));
            }

            if let Some(region) = &provider.region {
                config.push_str(&format!("region = \"{}\"\n", region));
            }

            if let Some(project_id) = &provider.project_id {
                config.push_str(&format!("project_id = \"{}\"\n", project_id));
            }

            config.push('\n');
        }
    }

    config
}

/// Escape a string for TOML output.
fn escape_toml_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Generate a fresh 256-bit URL-safe base64 session-signing secret. Called
/// from the wizard so a freshly-installed deployment has a stable secret
/// without the operator having to remember to set `SESSION_SECRET`.
///
/// Uses `OsRng` directly for an unambiguous CSPRNG sourced from the OS — the
/// `rand` 0.8 thread RNG is also CSPRNG-quality (ChaCha-seeded from the OS),
/// but pinning to `OsRng` is secure-by-construction and avoids the wizard
/// regressing if the `rand` defaults ever shift.
fn generate_session_secret() -> String {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use rand::{RngCore, rngs::OsRng};
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_escape_toml_string() {
        assert_eq!(escape_toml_string("hello"), "hello");
        assert_eq!(escape_toml_string("hello\"world"), "hello\\\"world");
        assert_eq!(escape_toml_string("path\\to\\file"), "path\\\\to\\\\file");
    }

    #[test]
    fn test_generate_minimal_config() {
        let wizard_config = WizardConfig {
            database: DatabaseConfig::Sqlite {
                path: "/tmp/test.db".to_string(),
            },
            cache: CacheConfig::Memory,
            providers: vec![ProviderConfig {
                provider_type: ProviderType::OpenAi,
                name: "openai".to_string(),
                api_key: Some("${OPENAI_API_KEY}".to_string()),
                base_url: None,
                region: None,
                project_id: None,
            }],
            auth: AuthModeConfig::None,
            rate_limits: RateLimitConfig::default(),
            budget: BudgetConfig::default(),
        };

        let config = generate_config(DeploymentMode::LocalDev, &wizard_config);

        assert!(config.contains("[server]"));
        assert!(config.contains("allow_loopback_urls = true"));
        assert!(config.contains("[database]"));
        assert!(config.contains("type = \"sqlite\""));
        assert!(config.contains("[cache]"));
        assert!(config.contains("type = \"memory\""));
        assert!(config.contains("[providers.openai]"));
        assert!(config.contains("api_key = \"${OPENAI_API_KEY}\""));
        assert!(config.contains("[auth.mode]"));
        assert!(config.contains("type = \"none\""));
    }

    #[test]
    fn test_generate_config_with_api_key_auth() {
        let wizard_config = WizardConfig {
            database: DatabaseConfig::Sqlite {
                path: "/tmp/test.db".to_string(),
            },
            cache: CacheConfig::Redis {
                url: "redis://localhost:6379".to_string(),
            },
            providers: vec![ProviderConfig {
                provider_type: ProviderType::Anthropic,
                name: "anthropic".to_string(),
                api_key: Some("${ANTHROPIC_API_KEY}".to_string()),
                base_url: None,
                region: None,
                project_id: None,
            }],
            auth: AuthModeConfig::ApiKey {
                key_prefix: "test_".to_string(),
            },
            rate_limits: RateLimitConfig {
                requests_per_minute: Some(100),
                tokens_per_minute: Some(50000),
                concurrent_requests: None,
            },
            budget: BudgetConfig {
                monthly_budget_usd: Some(Decimal::from(100)),
                daily_budget_usd: None,
            },
        };

        let config = generate_config(DeploymentMode::SingleNode, &wizard_config);

        assert!(!config.contains("allow_loopback_urls"));
        assert!(config.contains("[auth.mode]"));
        assert!(config.contains("type = \"api_key\""));
        assert!(config.contains("[auth.api_key]"));
        assert!(config.contains("key_prefix = \"test_\""));
        assert!(config.contains("[limits.rate_limits]"));
        assert!(config.contains("requests_per_minute = 100"));
        assert!(config.contains("tokens_per_minute = 50000"));
        assert!(config.contains("[limits.budgets]"));
        assert!(config.contains("monthly_budget_usd = \"100\""));
    }

    #[test]
    fn test_generate_config_with_oidc() {
        let wizard_config = WizardConfig {
            database: DatabaseConfig::Postgres {
                url: "postgres://localhost/gateway".to_string(),
            },
            cache: CacheConfig::Redis {
                url: "redis://localhost:6379".to_string(),
            },
            providers: vec![ProviderConfig {
                provider_type: ProviderType::OpenAi,
                name: "openai".to_string(),
                api_key: Some("${OPENAI_API_KEY}".to_string()),
                base_url: None,
                region: None,
                project_id: None,
            }],
            auth: AuthModeConfig::Idp(OidcConfig {
                issuer: "https://auth.example.com".to_string(),
                client_id: "my-app".to_string(),
                client_secret: "${OIDC_CLIENT_SECRET}".to_string(),
                redirect_uri: "http://localhost:8080/auth/callback".to_string(),
            }),
            rate_limits: RateLimitConfig::default(),
            budget: BudgetConfig::default(),
        };

        let config = generate_config(DeploymentMode::MultiNode, &wizard_config);

        assert!(config.contains("[auth.mode]"));
        assert!(config.contains("type = \"idp\""));
        assert!(config.contains("[auth.session]"));
        // OIDC settings are now comments (per-org SSO is configured via admin API)
        assert!(config.contains("# issuer = \"https://auth.example.com\""));
        assert!(config.contains("# client_id = \"my-app\""));
    }
}
