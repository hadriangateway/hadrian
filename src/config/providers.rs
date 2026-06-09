//! Provider configuration module.
//!
//! Providers are configured with custom names and specify their API type.
//! This allows multiple providers of the same type (e.g., multiple OpenAI-compatible
//! endpoints) with different configurations.
//!
//! # Example
//!
//! ```toml
//! [providers]
//! default_provider = "openrouter"
//!
//! [providers.openrouter]
//! type = "open_ai"
//! api_key = "${OPENROUTER_API_KEY}"
//! base_url = "https://openrouter.ai/api/v1/"
//!
//! [providers.anthropic-direct]
//! type = "anthropic"
//! api_key = "${ANTHROPIC_API_KEY}"
//!
//! [providers.local-ollama]
//! type = "open_ai"
//! base_url = "http://localhost:11434/v1"
//!
//! [providers.aws-claude]
//! type = "bedrock"
//! region = "us-east-1"
//! ```

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::ConfigError;
use crate::{
    catalog::{ModelCapabilities, ModelModalities},
    config::sovereignty::SovereigntyMetadata,
    pricing::ModelPricing,
};

/// Model-specific fallback configuration.
///
/// Specifies an alternative model to try when the primary model fails.
/// Can specify a different model on the same provider, or a different provider entirely.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ModelFallback {
    /// Model name to use for fallback.
    pub model: String,

    /// Provider name to use. If not specified, uses the same provider.
    #[serde(default)]
    pub provider: Option<String>,
}

/// Unified per-model configuration combining pricing, metadata, and task support.
///
/// Pricing fields are flattened inline so they can be specified directly:
/// ```toml
/// [providers.openai.models."dall-e-3"]
/// per_image = 40000
/// modalities = { input = ["text"], output = ["image"] }
/// tasks = ["image_generation"]
/// family = "dall-e"
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
// Note: cannot use deny_unknown_fields due to #[serde(flatten)] on `pricing`
pub struct ModelConfig {
    /// Pricing fields (flattened inline).
    #[serde(flatten)]
    pub pricing: ModelPricing,

    /// Input/output modalities (e.g., text, image, audio).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modalities: Option<ModelModalities>,

    /// Model capabilities (vision, reasoning, tool_call, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<ModelCapabilities>,

    /// Supported tasks / API endpoints (e.g., "chat", "image_generation", "tts",
    /// "transcription", "translation", "embedding").
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<String>,

    /// Maximum context window size (tokens).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_length: Option<i64>,

    /// Maximum output tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<i64>,

    /// Model family (e.g., "dall-e", "gpt-4", "whisper").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,

    /// Whether the model has open weights.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_weights: Option<bool>,

    /// Supported image sizes for image generation models.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub image_sizes: Vec<String>,

    /// Supported image quality options for image generation models.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub image_qualities: Vec<String>,

    /// Maximum number of images per request for image generation models.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_images: Option<i64>,

    /// Available voices for TTS models.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub voices: Vec<String>,

    /// Sovereignty and compliance metadata override for this model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sovereignty: Option<SovereigntyMetadata>,
}

/// Provider configurations container.
///
/// Each provider has a unique name (the TOML key) and specifies its type
/// to determine which API protocol to use.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
// Note: cannot use deny_unknown_fields due to #[serde(flatten)] on `providers` HashMap
pub struct ProvidersConfig {
    /// Default provider name for requests that don't specify one.
    #[serde(default)]
    pub default_provider: Option<String>,

    /// Provider configurations keyed by unique name.
    #[serde(flatten)]
    pub providers: HashMap<String, ProviderConfig>,
}

impl ProvidersConfig {
    pub fn validate(&self) -> Result<(), ConfigError> {
        // Validate default_provider exists if specified
        if let Some(default) = &self.default_provider
            && !self.providers.contains_key(default)
        {
            return Err(ConfigError::Validation(format!(
                "default_provider '{}' is not defined in providers",
                default
            )));
        }

        // Validate each provider
        for (name, config) in &self.providers {
            config
                .validate()
                .map_err(|e| ConfigError::Validation(format!("provider '{}': {}", name, e)))?;

            // Validate fallback_providers exist
            for fallback_name in config.fallback_providers() {
                if !self.providers.contains_key(fallback_name) {
                    return Err(ConfigError::Validation(format!(
                        "provider '{}': fallback provider '{}' is not defined",
                        name, fallback_name
                    )));
                }
                if fallback_name == name {
                    return Err(ConfigError::Validation(format!(
                        "provider '{}': cannot use self as fallback provider",
                        name
                    )));
                }
            }

            // Validate model_fallbacks reference valid providers
            for (model, fallbacks) in config.model_fallbacks() {
                for (idx, fallback) in fallbacks.iter().enumerate() {
                    if let Some(provider_name) = &fallback.provider
                        && !self.providers.contains_key(provider_name)
                    {
                        return Err(ConfigError::Validation(format!(
                            "provider '{}': model_fallbacks['{}'][{}].provider '{}' is not defined",
                            name, model, idx, provider_name
                        )));
                    }
                }
            }
        }

        Ok(())
    }

    /// Check if any providers are configured.
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    /// Get a provider by name.
    pub fn get(&self, name: &str) -> Option<&ProviderConfig> {
        self.providers.get(name)
    }

    /// Get the default provider configuration.
    pub fn get_default(&self) -> Option<(&str, &ProviderConfig)> {
        self.default_provider.as_ref().and_then(|name| {
            self.providers
                .get(name)
                .map(|config| (name.as_str(), config))
        })
    }

    /// Iterate over all providers.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &ProviderConfig)> {
        self.providers.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Find providers by type.
    pub fn providers_of_type(&self, provider_type: ProviderType) -> Vec<(&str, &ProviderConfig)> {
        self.providers
            .iter()
            .filter(|(_, config)| config.provider_type() == provider_type)
            .map(|(k, v)| (k.as_str(), v))
            .collect()
    }
}

/// Provider type identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProviderType {
    OpenAi,
    Anthropic,
    Bedrock,
    Vertex,
    AzureOpenAi,
    Test,
}

/// Configuration for a single provider.
///
/// The `type` field determines which API protocol to use.
/// Some providers require specific features to be enabled:
/// - `bedrock` requires the `provider-bedrock` feature
/// - `vertex` requires the `provider-vertex` feature
/// - `azure_openai` requires the `provider-azure` feature
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProviderConfig {
    /// OpenAI API (also works for OpenAI-compatible providers like OpenRouter,
    /// Together, Groq, and local servers like Ollama/vLLM).
    OpenAi(OpenAiProviderConfig),

    /// Anthropic API.
    Anthropic(AnthropicProviderConfig),

    /// AWS Bedrock. Requires the `provider-bedrock` feature.
    #[cfg(feature = "provider-bedrock")]
    Bedrock(BedrockProviderConfig),

    /// Google Vertex AI. Requires the `provider-vertex` feature.
    #[cfg(feature = "provider-vertex")]
    Vertex(VertexProviderConfig),

    /// Azure OpenAI. Requires the `provider-azure` feature.
    #[cfg(feature = "provider-azure")]
    AzureOpenAi(AzureOpenAiProviderConfig),

    /// Test provider (mock responses, no API calls).
    Test(TestProviderConfig),
}

impl ProviderConfig {
    /// Get the provider type.
    pub fn provider_type(&self) -> ProviderType {
        match self {
            Self::OpenAi(_) => ProviderType::OpenAi,
            Self::Anthropic(_) => ProviderType::Anthropic,
            #[cfg(feature = "provider-bedrock")]
            Self::Bedrock(_) => ProviderType::Bedrock,
            #[cfg(feature = "provider-vertex")]
            Self::Vertex(_) => ProviderType::Vertex,
            #[cfg(feature = "provider-azure")]
            Self::AzureOpenAi(_) => ProviderType::AzureOpenAi,
            Self::Test(_) => ProviderType::Test,
        }
    }

    fn validate(&self) -> Result<(), String> {
        match self {
            Self::OpenAi(c) => c.validate(),
            Self::Anthropic(c) => c.validate(),
            #[cfg(feature = "provider-bedrock")]
            Self::Bedrock(c) => c.validate(),
            #[cfg(feature = "provider-vertex")]
            Self::Vertex(c) => c.validate(),
            #[cfg(feature = "provider-azure")]
            Self::AzureOpenAi(c) => c.validate(),
            Self::Test(c) => c.validate(),
        }
    }

    /// Get the timeout for this provider in seconds.
    pub fn timeout_secs(&self) -> u64 {
        match self {
            Self::OpenAi(c) => c.timeout_secs,
            Self::Anthropic(c) => c.timeout_secs,
            #[cfg(feature = "provider-bedrock")]
            Self::Bedrock(c) => c.timeout_secs,
            #[cfg(feature = "provider-vertex")]
            Self::Vertex(c) => c.timeout_secs,
            #[cfg(feature = "provider-azure")]
            Self::AzureOpenAi(c) => c.timeout_secs,
            Self::Test(c) => c.timeout_secs,
        }
    }

    /// Get allowed models for this provider (empty means all models allowed).
    pub fn allowed_models(&self) -> &[String] {
        match self {
            Self::OpenAi(c) => &c.allowed_models,
            Self::Anthropic(c) => &c.allowed_models,
            #[cfg(feature = "provider-bedrock")]
            Self::Bedrock(c) => &c.allowed_models,
            #[cfg(feature = "provider-vertex")]
            Self::Vertex(c) => &c.allowed_models,
            #[cfg(feature = "provider-azure")]
            Self::AzureOpenAi(c) => &c.allowed_models,
            Self::Test(c) => &c.allowed_models,
        }
    }

    /// Get model aliases for this provider.
    pub fn model_aliases(&self) -> &HashMap<String, String> {
        match self {
            Self::OpenAi(c) => &c.model_aliases,
            Self::Anthropic(c) => &c.model_aliases,
            #[cfg(feature = "provider-bedrock")]
            Self::Bedrock(c) => &c.model_aliases,
            #[cfg(feature = "provider-vertex")]
            Self::Vertex(c) => &c.model_aliases,
            #[cfg(feature = "provider-azure")]
            Self::AzureOpenAi(c) => &c.model_aliases,
            Self::Test(c) => &c.model_aliases,
        }
    }

    /// Resolve a model alias if one exists.
    pub fn resolve_model<'a>(&'a self, model: &'a str) -> &'a str {
        self.model_aliases()
            .get(model)
            .map(|s| s.as_str())
            .unwrap_or(model)
    }

    /// Check if a model is allowed by this provider.
    pub fn is_model_allowed(&self, model: &str) -> bool {
        let allowed = self.allowed_models();
        if allowed.is_empty() {
            return true;
        }
        let resolved = self.resolve_model(model);
        allowed.iter().any(|m| m == resolved || m == model)
    }

    /// Get per-model configurations for this provider.
    pub fn models(&self) -> &HashMap<String, ModelConfig> {
        match self {
            Self::OpenAi(c) => &c.models,
            Self::Anthropic(c) => &c.models,
            #[cfg(feature = "provider-bedrock")]
            Self::Bedrock(c) => &c.models,
            #[cfg(feature = "provider-vertex")]
            Self::Vertex(c) => &c.models,
            #[cfg(feature = "provider-azure")]
            Self::AzureOpenAi(c) => &c.models,
            Self::Test(c) => &c.models,
        }
    }

    /// Get configuration for a specific model.
    pub fn get_model_config(&self, model: &str) -> Option<&ModelConfig> {
        self.models().get(model)
    }

    /// Get pricing for a specific model.
    pub fn get_pricing(&self, model: &str) -> Option<&ModelPricing> {
        self.get_model_config(model).map(|mc| &mc.pricing)
    }

    /// Get retry configuration for this provider.
    pub fn retry_config(&self) -> &RetryConfig {
        match self {
            Self::OpenAi(c) => &c.retry,
            Self::Anthropic(c) => &c.retry,
            #[cfg(feature = "provider-bedrock")]
            Self::Bedrock(c) => &c.retry,
            #[cfg(feature = "provider-vertex")]
            Self::Vertex(c) => &c.retry,
            #[cfg(feature = "provider-azure")]
            Self::AzureOpenAi(c) => &c.retry,
            Self::Test(c) => &c.retry,
        }
    }

    /// Get circuit breaker configuration for this provider.
    pub fn circuit_breaker_config(&self) -> &CircuitBreakerConfig {
        match self {
            Self::OpenAi(c) => &c.circuit_breaker,
            Self::Anthropic(c) => &c.circuit_breaker,
            #[cfg(feature = "provider-bedrock")]
            Self::Bedrock(c) => &c.circuit_breaker,
            #[cfg(feature = "provider-vertex")]
            Self::Vertex(c) => &c.circuit_breaker,
            #[cfg(feature = "provider-azure")]
            Self::AzureOpenAi(c) => &c.circuit_breaker,
            Self::Test(c) => &c.circuit_breaker,
        }
    }

    /// Get fallback provider names for this provider.
    ///
    /// Fallback providers are tried in order when the primary provider fails
    /// with a retryable error (5xx, timeout, circuit breaker open).
    pub fn fallback_providers(&self) -> &[String] {
        match self {
            Self::OpenAi(c) => &c.fallback_providers,
            Self::Anthropic(c) => &c.fallback_providers,
            #[cfg(feature = "provider-bedrock")]
            Self::Bedrock(c) => &c.fallback_providers,
            #[cfg(feature = "provider-vertex")]
            Self::Vertex(c) => &c.fallback_providers,
            #[cfg(feature = "provider-azure")]
            Self::AzureOpenAi(c) => &c.fallback_providers,
            Self::Test(c) => &c.fallback_providers,
        }
    }

    /// Get model-specific fallback configurations.
    ///
    /// Model fallbacks are tried before provider-level fallbacks and can specify
    /// alternative models on the same provider or different providers.
    pub fn model_fallbacks(&self) -> &HashMap<String, Vec<ModelFallback>> {
        match self {
            Self::OpenAi(c) => &c.model_fallbacks,
            Self::Anthropic(c) => &c.model_fallbacks,
            #[cfg(feature = "provider-bedrock")]
            Self::Bedrock(c) => &c.model_fallbacks,
            #[cfg(feature = "provider-vertex")]
            Self::Vertex(c) => &c.model_fallbacks,
            #[cfg(feature = "provider-azure")]
            Self::AzureOpenAi(c) => &c.model_fallbacks,
            Self::Test(c) => &c.model_fallbacks,
        }
    }

    /// Get fallbacks for a specific model.
    pub fn get_model_fallbacks(&self, model: &str) -> Option<&[ModelFallback]> {
        self.model_fallbacks().get(model).map(|v| v.as_slice())
    }

    /// Get streaming buffer configuration for this provider.
    ///
    /// Returns `Some` for providers that transform streams (Anthropic, Bedrock, Vertex)
    /// and `None` for providers that pass through streams as-is (OpenAI, Azure OpenAI, Test).
    ///
    /// Streaming buffer limits protect against DoS attacks from malformed SSE data
    /// (unbounded input without newlines) or slow consumers (unbounded output buffering).
    /// These limits are only needed when the gateway parses and re-emits stream events.
    pub fn streaming_buffer_config(&self) -> Option<&StreamingBufferConfig> {
        match self {
            Self::Anthropic(c) => Some(&c.streaming_buffer),
            #[cfg(feature = "provider-bedrock")]
            Self::Bedrock(c) => Some(&c.streaming_buffer),
            #[cfg(feature = "provider-vertex")]
            Self::Vertex(c) => Some(&c.streaming_buffer),
            // OpenAI-compatible providers pass through streams without transformation
            #[cfg(feature = "provider-azure")]
            Self::OpenAi(_) | Self::AzureOpenAi(_) | Self::Test(_) => None,
            #[cfg(not(feature = "provider-azure"))]
            Self::OpenAi(_) | Self::Test(_) => None,
        }
    }

    /// Get health check configuration for this provider.
    ///
    /// Health checks enable proactive monitoring of provider availability,
    /// complementing reactive circuit breakers.
    pub fn health_check_config(&self) -> &ProviderHealthCheckConfig {
        match self {
            Self::OpenAi(c) => &c.health_check,
            Self::Anthropic(c) => &c.health_check,
            #[cfg(feature = "provider-bedrock")]
            Self::Bedrock(c) => &c.health_check,
            #[cfg(feature = "provider-vertex")]
            Self::Vertex(c) => &c.health_check,
            #[cfg(feature = "provider-azure")]
            Self::AzureOpenAi(c) => &c.health_check,
            Self::Test(c) => &c.health_check,
        }
    }

    /// Get sovereignty metadata for this provider.
    pub fn sovereignty(&self) -> Option<&SovereigntyMetadata> {
        match self {
            Self::OpenAi(c) => c.sovereignty.as_ref(),
            Self::Anthropic(c) => c.sovereignty.as_ref(),
            #[cfg(feature = "provider-bedrock")]
            Self::Bedrock(c) => c.sovereignty.as_ref(),
            #[cfg(feature = "provider-vertex")]
            Self::Vertex(c) => c.sovereignty.as_ref(),
            #[cfg(feature = "provider-azure")]
            Self::AzureOpenAi(c) => c.sovereignty.as_ref(),
            Self::Test(c) => c.sovereignty.as_ref(),
        }
    }

    /// Get the catalog provider ID override for this provider.
    pub fn catalog_provider(&self) -> Option<&str> {
        match self {
            Self::OpenAi(c) => c.catalog_provider.as_deref(),
            Self::Anthropic(c) => c.catalog_provider.as_deref(),
            #[cfg(feature = "provider-bedrock")]
            Self::Bedrock(c) => c.catalog_provider.as_deref(),
            #[cfg(feature = "provider-vertex")]
            Self::Vertex(c) => c.catalog_provider.as_deref(),
            #[cfg(feature = "provider-azure")]
            Self::AzureOpenAi(c) => c.catalog_provider.as_deref(),
            Self::Test(c) => c.catalog_provider.as_deref(),
        }
    }

    /// Get the base URL for this provider (if applicable).
    /// Used for auto-detecting catalog provider from URL.
    pub fn base_url(&self) -> Option<&str> {
        match self {
            Self::OpenAi(c) => Some(&c.base_url),
            Self::Anthropic(c) => Some(&c.base_url),
            #[cfg(feature = "provider-bedrock")]
            Self::Bedrock(c) => c.converse_base_url.as_deref(),
            #[cfg(feature = "provider-vertex")]
            Self::Vertex(c) => c.base_url.as_deref(),
            #[cfg(feature = "provider-azure")]
            Self::AzureOpenAi(_) => None,
            Self::Test(_) => None,
        }
    }

    /// Get the provider type name as a string (for catalog lookup).
    pub fn provider_type_name(&self) -> &'static str {
        match self {
            Self::OpenAi(_) => "openai",
            Self::Anthropic(_) => "anthropic",
            #[cfg(feature = "provider-bedrock")]
            Self::Bedrock(_) => "bedrock",
            #[cfg(feature = "provider-vertex")]
            Self::Vertex(_) => "vertex",
            #[cfg(feature = "provider-azure")]
            Self::AzureOpenAi(_) => "azure_openai",
            Self::Test(_) => "test",
        }
    }
}

/// OpenAI-compatible provider configuration.
///
/// Works with the native OpenAI API as well as OpenAI-compatible providers:
/// - **OpenRouter**: `base_url = "https://openrouter.ai/api/v1/"`
/// - **Together AI**: `base_url = "https://api.together.xyz/v1/"`
/// - **Groq**: `base_url = "https://api.groq.com/openai/v1/"`
/// - **Ollama**: `base_url = "http://localhost:11434/v1/"`
/// - **vLLM**: `base_url = "http://localhost:8000/v1/"`
#[derive(Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct OpenAiProviderConfig {
    /// API key. Required for OpenAI and most hosted providers.
    /// Optional for local servers like Ollama.
    #[serde(default)]
    pub api_key: Option<String>,

    /// Base URL for the API.
    #[serde(default = "default_openai_base_url")]
    pub base_url: String,

    /// Organization ID (OpenAI-specific).
    #[serde(default)]
    pub organization: Option<String>,

    /// Project ID (OpenAI-specific, for project-based access).
    #[serde(default)]
    pub project: Option<String>,

    /// Request timeout in seconds.
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,

    /// Models available through this provider.
    /// If empty, all models are allowed.
    #[serde(default)]
    pub allowed_models: Vec<String>,

    /// Model aliases (e.g., "gpt4" -> "gpt-4-turbo-preview").
    #[serde(default)]
    pub model_aliases: HashMap<String, String>,

    /// Custom headers to include in requests.
    /// For OpenRouter providers, `HTTP-Referer`, `X-OpenRouter-Title`, and
    /// `X-OpenRouter-Categories` are set automatically for app attribution.
    /// Override here to customize or opt out.
    #[serde(default)]
    pub headers: HashMap<String, String>,

    /// Whether this provider supports function/tool calling.
    #[serde(default)]
    pub supports_tools: bool,

    /// Whether this provider supports vision/image inputs.
    #[serde(default)]
    pub supports_vision: bool,

    /// Per-model configuration (pricing, modalities, tasks, metadata).
    /// If set, overrides default pricing and adds metadata for these models.
    #[serde(default)]
    pub models: HashMap<String, ModelConfig>,

    /// Retry configuration for transient failures.
    #[serde(default)]
    pub retry: RetryConfig,

    /// Circuit breaker configuration for unhealthy provider protection.
    #[serde(default)]
    pub circuit_breaker: CircuitBreakerConfig,

    /// Fallback providers to try when this provider fails.
    /// Providers are tried in order on retryable errors (5xx, timeout, circuit breaker open).
    #[serde(default)]
    pub fallback_providers: Vec<String>,

    /// Model-specific fallback configurations.
    /// Model fallbacks are tried before provider-level fallbacks.
    #[serde(default)]
    pub model_fallbacks: HashMap<String, Vec<ModelFallback>>,

    /// Health check configuration for proactive provider monitoring.
    #[serde(default)]
    pub health_check: ProviderHealthCheckConfig,

    /// Override the catalog provider ID for model enrichment.
    /// If not specified, the provider is auto-detected from the base URL.
    /// Use this for OpenAI-compatible providers that aren't auto-detected.
    #[serde(default)]
    pub catalog_provider: Option<String>,

    /// Sovereignty and compliance metadata for this provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sovereignty: Option<SovereigntyMetadata>,
}

impl OpenAiProviderConfig {
    fn validate(&self) -> Result<(), String> {
        // Warn if using OpenAI's URL without an API key
        if self.base_url == default_openai_base_url() && self.api_key.is_none() {
            return Err("api_key is required for OpenAI's API".into());
        }
        Ok(())
    }

    /// Check if this is the native OpenAI API (not a compatible provider).
    pub fn is_native_openai(&self) -> bool {
        self.base_url.contains("api.openai.com")
    }
}

impl std::fmt::Debug for OpenAiProviderConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiProviderConfig")
            .field("api_key", &self.api_key.as_ref().map(|_| "****"))
            .field("base_url", &self.base_url)
            .field("organization", &self.organization)
            .field("project", &self.project)
            .field("timeout_secs", &self.timeout_secs)
            .field("allowed_models", &self.allowed_models)
            .field("model_aliases", &self.model_aliases)
            .field("headers", &self.headers)
            .field("supports_tools", &self.supports_tools)
            .field("supports_vision", &self.supports_vision)
            .field("models", &self.models)
            .field("retry", &self.retry)
            .field("circuit_breaker", &self.circuit_breaker)
            .field("fallback_providers", &self.fallback_providers)
            .field("model_fallbacks", &self.model_fallbacks)
            .field("health_check", &self.health_check)
            .field("catalog_provider", &self.catalog_provider)
            .field("sovereignty", &self.sovereignty)
            .finish()
    }
}

fn default_openai_base_url() -> String {
    "https://api.openai.com/v1".to_string()
}

/// Anthropic provider configuration.
#[derive(Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct AnthropicProviderConfig {
    /// API key (required).
    pub api_key: String,

    /// Base URL override.
    #[serde(default = "default_anthropic_base_url")]
    pub base_url: String,

    /// Request timeout in seconds.
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,

    /// Default model to use if not specified in requests.
    #[serde(default)]
    pub default_model: Option<String>,

    /// Default max_tokens to use if not specified in requests.
    #[serde(default)]
    pub default_max_tokens: Option<u32>,

    /// Models available through this provider.
    #[serde(default)]
    pub allowed_models: Vec<String>,

    /// Model aliases.
    #[serde(default)]
    pub model_aliases: HashMap<String, String>,

    /// Per-model configuration (pricing, modalities, tasks, metadata).
    #[serde(default)]
    pub models: HashMap<String, ModelConfig>,

    /// Retry configuration for transient failures.
    #[serde(default)]
    pub retry: RetryConfig,

    /// Circuit breaker configuration for unhealthy provider protection.
    #[serde(default)]
    pub circuit_breaker: CircuitBreakerConfig,

    /// Streaming buffer limits for DoS protection.
    #[serde(default)]
    pub streaming_buffer: StreamingBufferConfig,

    /// Fallback providers to try when this provider fails.
    #[serde(default)]
    pub fallback_providers: Vec<String>,

    /// Model-specific fallback configurations.
    #[serde(default)]
    pub model_fallbacks: HashMap<String, Vec<ModelFallback>>,

    /// Health check configuration for proactive provider monitoring.
    #[serde(default)]
    pub health_check: ProviderHealthCheckConfig,

    /// Override the catalog provider ID for model enrichment.
    /// Defaults to "anthropic".
    #[serde(default)]
    pub catalog_provider: Option<String>,

    /// Sovereignty and compliance metadata for this provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sovereignty: Option<SovereigntyMetadata>,

    /// Models for which the `interleaved-thinking-2025-05-14` beta header
    /// should be sent when thinking is enabled. Each entry is matched against
    /// the model name as a substring (e.g. `"opus-4-6"` matches
    /// `"claude-opus-4-6-20250101"`). Some Anthropic models reject this
    /// header, so override the default list when adding or removing support.
    /// Set to an empty list to disable the beta header entirely.
    ///
    /// Note: Opus 4.7/4.8 are deliberately **excluded** — adaptive thinking
    /// auto-enables interleaved thinking on those models, and they reject the
    /// explicit header. Only models still relying on the legacy header belong
    /// here.
    #[serde(default = "default_interleaved_thinking_models")]
    pub interleaved_thinking_models: Vec<String>,

    /// Models that use **adaptive thinking** (Claude Opus 4.6+ and Sonnet 4.6).
    /// Substring match against the model name. Adaptive-capable models receive
    /// `thinking: {type: "adaptive"}` + `output_config.effort` instead of a
    /// fixed `budget_tokens`. Models not listed here fall back to the legacy
    /// budget path. Extend this list as new adaptive models ship.
    #[serde(default = "default_adaptive_thinking_models")]
    pub adaptive_thinking_models: Vec<String>,

    /// Models that **forbid** `budget_tokens` and sampling parameters
    /// (`temperature`/`top_p`/`top_k`) and that support `thinking.display`
    /// (Claude Opus 4.7/4.8). Substring match. These always use adaptive
    /// thinking — the legacy budget path is never taken for them, even if a
    /// request supplies `reasoning.max_tokens`.
    #[serde(default = "default_strict_thinking_models")]
    pub strict_thinking_models: Vec<String>,

    /// Models that support **mid-conversation system messages** — an inline
    /// `role:"system"` message in the `messages` array (Claude Opus 4.8 only at
    /// time of writing; no beta header is required). Substring match. For models
    /// not listed here, system/developer messages that appear after the first
    /// turn are folded into the top-level `system` prompt, since those models
    /// reject a non-user/assistant role in `messages`. Extend this list as new
    /// models gain support.
    #[serde(default = "default_mid_conversation_system_models")]
    pub mid_conversation_system_models: Vec<String>,
}

pub fn default_interleaved_thinking_models() -> Vec<String> {
    vec!["opus-4-6".to_string(), "opus-4.6".to_string()]
}

pub fn default_adaptive_thinking_models() -> Vec<String> {
    vec![
        "opus-4-6".to_string(),
        "opus-4.6".to_string(),
        "opus-4-7".to_string(),
        "opus-4.7".to_string(),
        "opus-4-8".to_string(),
        "opus-4.8".to_string(),
        "sonnet-4-6".to_string(),
        "sonnet-4.6".to_string(),
    ]
}

pub fn default_strict_thinking_models() -> Vec<String> {
    vec![
        "opus-4-7".to_string(),
        "opus-4.7".to_string(),
        "opus-4-8".to_string(),
        "opus-4.8".to_string(),
    ]
}

pub fn default_mid_conversation_system_models() -> Vec<String> {
    vec!["opus-4-8".to_string(), "opus-4.8".to_string()]
}

impl AnthropicProviderConfig {
    fn validate(&self) -> Result<(), String> {
        if self.api_key.is_empty() {
            return Err("api_key cannot be empty".into());
        }
        Ok(())
    }
}

impl std::fmt::Debug for AnthropicProviderConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnthropicProviderConfig")
            .field("api_key", &"****")
            .field("base_url", &self.base_url)
            .field("timeout_secs", &self.timeout_secs)
            .field("default_model", &self.default_model)
            .field("default_max_tokens", &self.default_max_tokens)
            .field("allowed_models", &self.allowed_models)
            .field("model_aliases", &self.model_aliases)
            .field("models", &self.models)
            .field("retry", &self.retry)
            .field("circuit_breaker", &self.circuit_breaker)
            .field("streaming_buffer", &self.streaming_buffer)
            .field("fallback_providers", &self.fallback_providers)
            .field("model_fallbacks", &self.model_fallbacks)
            .field("health_check", &self.health_check)
            .field("catalog_provider", &self.catalog_provider)
            .field("sovereignty", &self.sovereignty)
            .finish()
    }
}

fn default_anthropic_base_url() -> String {
    "https://api.anthropic.com".to_string()
}

#[cfg(feature = "provider-bedrock")]
/// AWS Bedrock provider configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BedrockProviderConfig {
    /// AWS region.
    pub region: String,

    /// Credential source.
    #[serde(default)]
    pub credentials: AwsCredentials,

    /// Request timeout in seconds.
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,

    /// Models available through this provider.
    #[serde(default)]
    pub allowed_models: Vec<String>,

    /// Model aliases.
    #[serde(default)]
    pub model_aliases: HashMap<String, String>,

    /// Cross-region inference profile ARN (for multi-region routing).
    #[serde(default)]
    pub inference_profile_arn: Option<String>,

    /// Per-model configuration (pricing, modalities, tasks, metadata).
    #[serde(default)]
    pub models: HashMap<String, ModelConfig>,

    /// Retry configuration for transient failures.
    #[serde(default)]
    pub retry: RetryConfig,

    /// Circuit breaker configuration for unhealthy provider protection.
    #[serde(default)]
    pub circuit_breaker: CircuitBreakerConfig,

    /// Streaming buffer limits for DoS protection.
    #[serde(default)]
    pub streaming_buffer: StreamingBufferConfig,

    /// Fallback providers to try when this provider fails.
    #[serde(default)]
    pub fallback_providers: Vec<String>,

    /// Model-specific fallback configurations.
    #[serde(default)]
    pub model_fallbacks: HashMap<String, Vec<ModelFallback>>,

    /// Custom Converse API base URL override.
    /// If not specified, defaults to `https://bedrock-runtime.<region>.amazonaws.com`.
    /// This is useful for VPC endpoints, testing, or custom deployments.
    #[serde(default)]
    pub converse_base_url: Option<String>,

    /// Health check configuration for proactive provider monitoring.
    #[serde(default)]
    pub health_check: ProviderHealthCheckConfig,

    /// Override the catalog provider ID for model enrichment.
    /// Defaults to "amazon-bedrock".
    #[serde(default)]
    pub catalog_provider: Option<String>,

    /// Sovereignty and compliance metadata for this provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sovereignty: Option<SovereigntyMetadata>,

    /// Substring allowlist of Bedrock-hosted Claude models that should
    /// receive the `interleaved-thinking-2025-05-14` beta header when
    /// adaptive thinking is requested. Some Bedrock-hosted Claude models
    /// reject the header, so this lets operators opt models in/out without
    /// recompiling. Set to an empty list to disable the beta header.
    /// Mirrors `AnthropicProviderConfig.interleaved_thinking_models`.
    #[serde(default = "default_interleaved_thinking_models")]
    pub interleaved_thinking_models: Vec<String>,
}

#[cfg(feature = "provider-bedrock")]
impl BedrockProviderConfig {
    fn validate(&self) -> Result<(), String> {
        if self.region.is_empty() {
            return Err("region cannot be empty".into());
        }
        Ok(())
    }
}

#[cfg(feature = "provider-bedrock")]
/// AWS credential configuration.
#[derive(Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
pub enum AwsCredentials {
    /// Use the default credential chain (env, profile, IMDS, etc.)
    #[default]
    Default,

    /// Use static credentials.
    Static {
        access_key_id: String,
        secret_access_key: String,
        #[serde(default)]
        session_token: Option<String>,
    },

    /// Assume an IAM role.
    AssumeRole {
        role_arn: String,
        #[serde(default)]
        external_id: Option<String>,
        #[serde(default)]
        session_name: Option<String>,
    },

    /// Use a specific AWS profile.
    Profile { name: String },
}

#[cfg(feature = "provider-bedrock")]
impl std::fmt::Debug for AwsCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AwsCredentials::Default => write!(f, "Default"),
            AwsCredentials::Static { .. } => f
                .debug_struct("Static")
                .field("access_key_id", &"****")
                .field("secret_access_key", &"****")
                .field("session_token", &"****")
                .finish(),
            AwsCredentials::AssumeRole {
                role_arn,
                external_id,
                session_name,
            } => f
                .debug_struct("AssumeRole")
                .field("role_arn", role_arn)
                .field("external_id", external_id)
                .field("session_name", session_name)
                .finish(),
            AwsCredentials::Profile { name } => {
                f.debug_struct("Profile").field("name", name).finish()
            }
        }
    }
}

#[cfg(feature = "provider-vertex")]
/// Google Vertex AI provider configuration.
///
/// Supports two authentication modes:
///
/// **1. API Key mode** (simple, recommended for getting started):
/// ```toml
/// [providers.gemini]
/// type = "vertex"
/// api_key = "${GOOGLE_API_KEY}"
/// ```
///
/// **2. OAuth/ADC mode** (for full Vertex AI features):
/// ```toml
/// [providers.vertex]
/// type = "vertex"
/// project = "my-project"
/// region = "us-central1"
/// publisher = "google"  # or "anthropic", "meta"
/// ```
#[derive(Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct VertexProviderConfig {
    /// API key for simple Gemini API access.
    /// When set, uses `https://aiplatform.googleapis.com/v1/publishers/{publisher}/models`
    /// with `?key=` query parameter authentication.
    /// Mutually exclusive with project/region/credentials.
    #[serde(default)]
    pub api_key: Option<String>,

    /// GCP project ID. Required for OAuth/ADC mode, ignored with api_key.
    #[serde(default)]
    pub project: Option<String>,

    /// GCP region. Required for OAuth/ADC mode, ignored with api_key.
    #[serde(default)]
    pub region: Option<String>,

    /// Model publisher. Defaults to "google".
    /// Use "anthropic" for Claude models, "meta" for Llama models on Vertex AI.
    #[serde(default = "default_vertex_publisher")]
    pub publisher: String,

    /// Custom base URL override.
    /// Useful for VPC endpoints, testing, or custom deployments.
    /// If not specified, defaults based on auth mode:
    /// - API key: `https://aiplatform.googleapis.com`
    /// - OAuth: `https://{region}-aiplatform.googleapis.com`
    #[serde(default)]
    pub base_url: Option<String>,

    /// Credential source for OAuth/ADC mode. Ignored with api_key.
    #[serde(default)]
    pub credentials: GcpCredentials,

    /// Request timeout in seconds.
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,

    /// Models available through this provider.
    #[serde(default)]
    pub allowed_models: Vec<String>,

    /// Model aliases.
    #[serde(default)]
    pub model_aliases: HashMap<String, String>,

    /// Per-model configuration (pricing, modalities, tasks, metadata).
    #[serde(default)]
    pub models: HashMap<String, ModelConfig>,

    /// Retry configuration for transient failures.
    #[serde(default)]
    pub retry: RetryConfig,

    /// Circuit breaker configuration for unhealthy provider protection.
    #[serde(default)]
    pub circuit_breaker: CircuitBreakerConfig,

    /// Streaming buffer limits for DoS protection.
    #[serde(default)]
    pub streaming_buffer: StreamingBufferConfig,

    /// Fallback providers to try when this provider fails.
    #[serde(default)]
    pub fallback_providers: Vec<String>,

    /// Model-specific fallback configurations.
    #[serde(default)]
    pub model_fallbacks: HashMap<String, Vec<ModelFallback>>,

    /// Health check configuration for proactive provider monitoring.
    #[serde(default)]
    pub health_check: ProviderHealthCheckConfig,

    /// Override the catalog provider ID for model enrichment.
    /// Defaults to "google-vertex".
    #[serde(default)]
    pub catalog_provider: Option<String>,

    /// Sovereignty and compliance metadata for this provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sovereignty: Option<SovereigntyMetadata>,
}

#[cfg(feature = "provider-vertex")]
impl VertexProviderConfig {
    fn validate(&self) -> Result<(), String> {
        // Either api_key OR (project + region) must be provided
        if self.api_key.is_some() {
            // API key mode - project/region are optional (ignored)
            Ok(())
        } else {
            // OAuth/ADC mode - project and region are required
            match (&self.project, &self.region) {
                (Some(p), Some(r)) if !p.is_empty() && !r.is_empty() => Ok(()),
                (Some(p), _) if p.is_empty() => Err("project cannot be empty".into()),
                (_, Some(r)) if r.is_empty() => Err("region cannot be empty".into()),
                _ => Err("either api_key or both project and region must be provided".into()),
            }
        }
    }

    /// Check if using API key authentication mode.
    pub fn uses_api_key(&self) -> bool {
        self.api_key.is_some()
    }
}

#[cfg(feature = "provider-vertex")]
impl std::fmt::Debug for VertexProviderConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VertexProviderConfig")
            .field("api_key", &self.api_key.as_ref().map(|_| "****"))
            .field("project", &self.project)
            .field("region", &self.region)
            .field("publisher", &self.publisher)
            .field("base_url", &self.base_url)
            .field("credentials", &self.credentials)
            .field("timeout_secs", &self.timeout_secs)
            .field("allowed_models", &self.allowed_models)
            .field("model_aliases", &self.model_aliases)
            .field("models", &self.models)
            .field("retry", &self.retry)
            .field("circuit_breaker", &self.circuit_breaker)
            .field("streaming_buffer", &self.streaming_buffer)
            .field("fallback_providers", &self.fallback_providers)
            .field("model_fallbacks", &self.model_fallbacks)
            .field("health_check", &self.health_check)
            .field("catalog_provider", &self.catalog_provider)
            .field("sovereignty", &self.sovereignty)
            .finish()
    }
}

#[cfg(feature = "provider-vertex")]
fn default_vertex_publisher() -> String {
    "google".to_string()
}

#[cfg(feature = "provider-vertex")]
/// GCP credential configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
pub enum GcpCredentials {
    /// Use Application Default Credentials.
    #[default]
    Default,

    /// Use a service account key file.
    ServiceAccount { key_path: String },

    /// Use a service account key from JSON string (useful with env vars).
    ServiceAccountJson { json: String },
}

#[cfg(feature = "provider-azure")]
/// Azure OpenAI provider configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct AzureOpenAiProviderConfig {
    /// Azure resource name.
    pub resource_name: String,

    /// API version.
    #[serde(default = "default_azure_api_version")]
    pub api_version: String,

    /// Authentication method.
    pub auth: AzureAuth,

    /// Request timeout in seconds.
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,

    /// Deployment configurations.
    /// Maps deployment ID to model info for routing.
    #[serde(default)]
    pub deployments: HashMap<String, AzureDeployment>,

    /// Models available through this provider.
    #[serde(default)]
    pub allowed_models: Vec<String>,

    /// Model aliases.
    #[serde(default)]
    pub model_aliases: HashMap<String, String>,

    /// Per-model configuration (pricing, modalities, tasks, metadata).
    #[serde(default)]
    pub models: HashMap<String, ModelConfig>,

    /// Retry configuration for transient failures.
    #[serde(default)]
    pub retry: RetryConfig,

    /// Circuit breaker configuration for unhealthy provider protection.
    #[serde(default)]
    pub circuit_breaker: CircuitBreakerConfig,

    /// Fallback providers to try when this provider fails.
    #[serde(default)]
    pub fallback_providers: Vec<String>,

    /// Model-specific fallback configurations.
    #[serde(default)]
    pub model_fallbacks: HashMap<String, Vec<ModelFallback>>,

    /// Health check configuration for proactive provider monitoring.
    #[serde(default)]
    pub health_check: ProviderHealthCheckConfig,

    /// Override the catalog provider ID for model enrichment.
    /// Defaults to "azure".
    #[serde(default)]
    pub catalog_provider: Option<String>,

    /// Sovereignty and compliance metadata for this provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sovereignty: Option<SovereigntyMetadata>,
}

#[cfg(feature = "provider-azure")]
impl AzureOpenAiProviderConfig {
    fn validate(&self) -> Result<(), String> {
        if self.resource_name.is_empty() {
            return Err("resource_name cannot be empty".into());
        }
        Ok(())
    }

    /// Get the base URL for this Azure OpenAI resource.
    pub fn base_url(&self) -> String {
        format!("https://{}.openai.azure.com/openai", self.resource_name)
    }

    /// Find a deployment by model name.
    pub fn deployment_for_model(&self, model: &str) -> Option<(&str, &AzureDeployment)> {
        self.deployments
            .iter()
            .find(|(_, d)| d.model == model)
            .map(|(k, v)| (k.as_str(), v))
    }
}

#[cfg(feature = "provider-azure")]
fn default_azure_api_version() -> String {
    "2024-02-01".to_string()
}

#[cfg(feature = "provider-azure")]
/// Azure deployment configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct AzureDeployment {
    /// Model name this deployment serves (for routing).
    pub model: String,

    /// Whether this is the default deployment for the model.
    #[serde(default)]
    pub default: bool,
}

#[cfg(feature = "provider-azure")]
/// Azure authentication configuration.
#[derive(Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
pub enum AzureAuth {
    /// API key authentication.
    ApiKey { api_key: String },

    /// Azure AD / Entra ID authentication.
    AzureAd {
        /// Tenant ID.
        tenant_id: String,
        /// Client ID.
        client_id: String,
        /// Client secret.
        client_secret: String,
    },

    /// Managed identity authentication.
    ManagedIdentity {
        /// Client ID of the managed identity (optional for system-assigned).
        #[serde(default)]
        client_id: Option<String>,
    },
}

#[cfg(feature = "provider-azure")]
impl std::fmt::Debug for AzureAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AzureAuth::ApiKey { .. } => f.debug_struct("ApiKey").field("api_key", &"****").finish(),
            AzureAuth::AzureAd {
                tenant_id,
                client_id,
                ..
            } => f
                .debug_struct("AzureAd")
                .field("tenant_id", tenant_id)
                .field("client_id", client_id)
                .field("client_secret", &"****")
                .finish(),
            AzureAuth::ManagedIdentity { client_id } => f
                .debug_struct("ManagedIdentity")
                .field("client_id", client_id)
                .finish(),
        }
    }
}

fn default_timeout() -> u64 {
    300 // 5 minutes
}

/// Configuration for provider request retries.
///
/// When enabled, retries requests on transient failures with exponential backoff.
/// Only retries on status codes that indicate temporary issues (429, 5xx).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RetryConfig {
    /// Whether retries are enabled.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Maximum number of retry attempts (not including the initial request).
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,

    /// Initial delay before first retry in milliseconds.
    #[serde(default = "default_initial_delay_ms")]
    pub initial_delay_ms: u64,

    /// Maximum delay between retries in milliseconds.
    #[serde(default = "default_max_delay_ms")]
    pub max_delay_ms: u64,

    /// Multiplier for exponential backoff.
    #[serde(default = "default_backoff_multiplier")]
    pub backoff_multiplier: f64,

    /// Add random jitter to delays (percentage, 0.0-1.0).
    #[serde(default = "default_jitter")]
    pub jitter: f64,

    /// Status codes that should trigger a retry.
    /// Default: 429 (rate limit), 500, 502, 503, 504 (server errors).
    #[serde(default = "default_retryable_status_codes")]
    pub retryable_status_codes: Vec<u16>,

    /// Override max_retries for embedding operations.
    /// Embeddings are fully idempotent (same input = same output), so aggressive retry is safe.
    /// Default: 5
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_max_retries: Option<u32>,

    /// Override max_retries for image generation operations.
    /// Image generation is NOT idempotent (each attempt creates a different image),
    /// so we minimize retries to avoid creating duplicates.
    /// Default: 1
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_generation_max_retries: Option<u32>,

    /// Override max_retries for read-only operations (list_models, etc.).
    /// These are fully idempotent with no side effects, so aggressive retry is safe.
    /// Default: 5
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_only_max_retries: Option<u32>,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_retries: default_max_retries(),
            initial_delay_ms: default_initial_delay_ms(),
            max_delay_ms: default_max_delay_ms(),
            backoff_multiplier: default_backoff_multiplier(),
            jitter: default_jitter(),
            retryable_status_codes: default_retryable_status_codes(),
            embedding_max_retries: None,
            image_generation_max_retries: None,
            read_only_max_retries: None,
        }
    }
}

/// Default retry count for embedding operations.
/// Embeddings are fully idempotent (same input = same output), so aggressive retry is safe.
const EMBEDDING_MAX_RETRIES: u32 = 5;

/// Default retry count for image generation operations.
/// Image generation is not idempotent (each attempt creates a different image),
/// so we minimize retries to avoid creating duplicates.
const IMAGE_GENERATION_MAX_RETRIES: u32 = 1;

/// Default retry count for read-only operations (list_models, etc.).
/// These are fully idempotent and have no side effects.
const READ_ONLY_MAX_RETRIES: u32 = 5;

impl RetryConfig {
    /// Check if a status code should trigger a retry.
    pub fn should_retry_status(&self, status: u16) -> bool {
        self.enabled && self.retryable_status_codes.contains(&status)
    }

    /// Calculate the delay for a given retry attempt (0-indexed).
    pub fn delay_for_attempt(&self, attempt: u32) -> std::time::Duration {
        let base_delay =
            (self.initial_delay_ms as f64) * self.backoff_multiplier.powi(attempt as i32);
        let capped_delay = base_delay.min(self.max_delay_ms as f64);

        // Add jitter
        let jitter_range = capped_delay * self.jitter;
        let jitter = if jitter_range > 0.0 {
            use rand::Rng;
            rand::thread_rng().gen_range(-jitter_range..jitter_range)
        } else {
            0.0
        };

        let final_delay = (capped_delay + jitter).max(0.0);
        std::time::Duration::from_millis(final_delay as u64)
    }

    /// Create a new config with a different max_retries value.
    /// Returns `Cow::Borrowed` if the value is unchanged, `Cow::Owned` otherwise.
    fn with_max_retries(&self, max_retries: u32) -> std::borrow::Cow<'_, Self> {
        if self.max_retries == max_retries {
            std::borrow::Cow::Borrowed(self)
        } else {
            std::borrow::Cow::Owned(Self {
                max_retries,
                ..self.clone()
            })
        }
    }

    /// Get retry config optimized for embedding operations.
    ///
    /// Embeddings are fully idempotent (same input produces identical output),
    /// so aggressive retry is safe. Uses configured `embedding_max_retries` or 5 by default.
    pub fn for_embedding(&self) -> std::borrow::Cow<'_, Self> {
        let target = self.embedding_max_retries.unwrap_or(EMBEDDING_MAX_RETRIES);
        self.with_max_retries(target)
    }

    /// Get retry config optimized for image generation operations.
    ///
    /// Image generation is NOT idempotent - each attempt creates a different image.
    /// We minimize retries to avoid creating duplicate images.
    /// Uses configured `image_generation_max_retries` or 1 by default.
    pub fn for_image_generation(&self) -> std::borrow::Cow<'_, Self> {
        let target = self
            .image_generation_max_retries
            .unwrap_or(IMAGE_GENERATION_MAX_RETRIES);
        self.with_max_retries(target)
    }

    /// Get retry config optimized for read-only operations (list_models, etc.).
    ///
    /// Read-only operations are fully idempotent with no side effects,
    /// so aggressive retry is safe. Uses configured `read_only_max_retries` or 5 by default.
    pub fn for_read_only(&self) -> std::borrow::Cow<'_, Self> {
        let target = self.read_only_max_retries.unwrap_or(READ_ONLY_MAX_RETRIES);
        self.with_max_retries(target)
    }
}

fn default_max_retries() -> u32 {
    3
}

fn default_initial_delay_ms() -> u64 {
    100
}

fn default_max_delay_ms() -> u64 {
    10_000
}

fn default_backoff_multiplier() -> f64 {
    2.0
}

fn default_jitter() -> f64 {
    0.1
}

fn default_retryable_status_codes() -> Vec<u16> {
    vec![429, 500, 502, 503, 504]
}

fn default_true() -> bool {
    true
}

/// Configuration for streaming response buffer limits.
///
/// These limits prevent DoS attacks from malformed SSE data or slow consumers.
/// Only applies to providers that transform streams (Anthropic, Bedrock, Vertex).
///
/// OpenAI-compatible providers (OpenAI, Azure OpenAI) pass through SSE streams
/// directly without buffering or transformation, so these limits don't apply.
///
/// Use [`ProviderConfig::streaming_buffer_config()`] to check if a provider
/// supports streaming buffer configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct StreamingBufferConfig {
    /// Maximum size of the input buffer in bytes.
    /// Protects against malformed SSE data without newlines.
    /// Default: 16 MB
    #[serde(default = "default_max_input_buffer_bytes")]
    pub max_input_buffer_bytes: usize,

    /// Maximum number of output chunks to buffer.
    /// Protects against slow consumers causing unbounded memory growth.
    /// Default: 1000 chunks
    #[serde(default = "default_max_output_buffer_chunks")]
    pub max_output_buffer_chunks: usize,

    /// Maximum total bytes of accumulated response state (text and reasoning
    /// content) per stream. Bounds memory usage if a provider produces a
    /// runaway response. Bytes beyond this cap are silently dropped from the
    /// state buffer, but pass-through deltas are still emitted to the client.
    /// Default: 32 MB
    #[serde(default = "default_max_response_state_bytes")]
    pub max_response_state_bytes: usize,
}

impl Default for StreamingBufferConfig {
    fn default() -> Self {
        Self {
            max_input_buffer_bytes: default_max_input_buffer_bytes(),
            max_output_buffer_chunks: default_max_output_buffer_chunks(),
            max_response_state_bytes: default_max_response_state_bytes(),
        }
    }
}

fn default_max_input_buffer_bytes() -> usize {
    16 * 1024 * 1024 // 16 MB
}

fn default_max_response_state_bytes() -> usize {
    32 * 1024 * 1024 // 32 MB
}

fn default_max_output_buffer_chunks() -> usize {
    1000
}

/// Configuration for circuit breaker pattern on providers.
///
/// The circuit breaker prevents hammering unhealthy providers by tracking failures
/// and temporarily rejecting requests after a threshold is exceeded.
///
/// States:
/// - **Closed**: Normal operation, requests pass through. Failures are tracked.
/// - **Open**: After threshold failures, requests are rejected immediately.
/// - **Half-Open**: After timeout, limited probe requests are allowed to test recovery.
///
/// # Adaptive Backoff
///
/// When a provider repeatedly fails (circuit opens, half-open probe fails, circuit reopens),
/// the open timeout increases exponentially to avoid hammering an unhealthy provider:
///
/// ```text
/// timeout = min(open_timeout_secs * backoff_multiplier^consecutive_opens, max_open_timeout_secs)
/// ```
///
/// For example, with defaults (30s base, 2.0 multiplier, 300s max):
/// - First open: 30s
/// - Second open (probe failed): 60s
/// - Third open: 120s
/// - Fourth open: 240s
/// - Fifth+ open: 300s (capped)
///
/// The counter resets when the circuit successfully closes (provider recovers).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct CircuitBreakerConfig {
    /// Whether circuit breaker is enabled.
    #[serde(default)]
    pub enabled: bool,

    /// Number of consecutive failures to trigger the circuit breaker.
    #[serde(default = "default_failure_threshold")]
    pub failure_threshold: u32,

    /// Base duration in seconds to keep the circuit open before attempting recovery.
    /// This is the initial timeout; subsequent opens may be longer due to adaptive backoff.
    #[serde(default = "default_open_timeout_secs")]
    pub open_timeout_secs: u64,

    /// Number of successful probe requests required to close the circuit.
    #[serde(default = "default_success_threshold")]
    pub success_threshold: u32,

    /// Status codes that count as failures for the circuit breaker.
    /// Default: 500, 502, 503, 504 (server errors). Note: 429 is NOT included
    /// because rate limits are expected behavior, not provider failure.
    #[serde(default = "default_circuit_breaker_failure_codes")]
    pub failure_status_codes: Vec<u16>,

    /// Multiplier for exponential backoff on repeated circuit opens.
    /// When a half-open probe fails, the next open timeout is multiplied by this value.
    /// Set to 1.0 to disable adaptive backoff (fixed timeout).
    #[serde(default = "default_backoff_multiplier")]
    pub backoff_multiplier: f64,

    /// Maximum open timeout in seconds after repeated failures.
    /// Caps the exponential backoff to prevent excessively long waits.
    #[serde(default = "default_max_open_timeout_secs")]
    pub max_open_timeout_secs: u64,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            failure_threshold: default_failure_threshold(),
            open_timeout_secs: default_open_timeout_secs(),
            success_threshold: default_success_threshold(),
            failure_status_codes: default_circuit_breaker_failure_codes(),
            backoff_multiplier: default_backoff_multiplier(),
            max_open_timeout_secs: default_max_open_timeout_secs(),
        }
    }
}

impl CircuitBreakerConfig {
    /// Check if a status code counts as a failure.
    pub fn is_failure_status(&self, status: u16) -> bool {
        self.failure_status_codes.contains(&status)
    }

    /// Calculate the open timeout for a given number of consecutive opens.
    ///
    /// Uses exponential backoff: `min(base * multiplier^consecutive_opens, max)`
    pub fn calculate_open_timeout_secs(&self, consecutive_opens: u32) -> u64 {
        if consecutive_opens == 0 || self.backoff_multiplier <= 1.0 {
            return self.open_timeout_secs;
        }

        let multiplied = (self.open_timeout_secs as f64)
            * self.backoff_multiplier.powi(consecutive_opens as i32);

        (multiplied as u64).min(self.max_open_timeout_secs)
    }
}

fn default_failure_threshold() -> u32 {
    5
}

fn default_open_timeout_secs() -> u64 {
    30
}

fn default_success_threshold() -> u32 {
    2
}

fn default_circuit_breaker_failure_codes() -> Vec<u16> {
    vec![500, 502, 503, 504]
}

fn default_max_open_timeout_secs() -> u64 {
    300 // 5 minutes
}

// =============================================================================
// Provider Health Check Configuration
// =============================================================================

/// Default provider health check interval in seconds.
pub const DEFAULT_PROVIDER_HEALTH_CHECK_INTERVAL_SECS: u64 = 60;

/// Default provider health check timeout in seconds.
pub const DEFAULT_PROVIDER_HEALTH_CHECK_TIMEOUT_SECS: u64 = 10;

/// Default prompt for inference health checks.
pub const DEFAULT_PROVIDER_HEALTH_CHECK_PROMPT: &str = "ping";

/// Health check mode determining how provider health is verified.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ProviderHealthCheckMode {
    /// Call the provider's list models endpoint.
    /// Free, fast, verifies connectivity and authentication.
    #[default]
    Reachability,

    /// Send a minimal chat completion request.
    /// More thorough but costs money. Requires a model to be specified.
    Inference,
}

/// Configuration for provider health checks.
///
/// Health checks allow proactive monitoring of provider availability,
/// rather than only reacting to failures via circuit breakers.
///
/// # Modes
///
/// - **Reachability** (default): Calls the provider's `/models` endpoint.
///   This is free, fast, and verifies basic connectivity and authentication.
///
/// - **Inference**: Sends a minimal chat completion request.
///   More thorough (tests the full inference path) but costs money.
///   Requires specifying a model and optional prompt.
///
/// # Example
///
/// ```toml
/// [providers.my-openai.health_check]
/// enabled = true
/// mode = "reachability"  # or "inference"
/// interval_secs = 60     # Check every 60 seconds
/// timeout_secs = 10      # Timeout for health check request
///
/// # Only for mode = "inference"
/// model = "gpt-4o-mini"  # Cheap model for health checks
/// prompt = "Say OK"      # Simple prompt (default: "ping")
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(default, deny_unknown_fields)]
pub struct ProviderHealthCheckConfig {
    /// Whether health checks are enabled for this provider.
    /// Default: false (no active health checks)
    pub enabled: bool,

    /// Health check mode.
    /// Default: Reachability (free endpoint check)
    pub mode: ProviderHealthCheckMode,

    /// Interval between health checks in seconds.
    /// Default: 60 seconds
    pub interval_secs: u64,

    /// Timeout for each health check request in seconds.
    /// Default: 10 seconds
    pub timeout_secs: u64,

    /// Model to use for inference health checks.
    /// Required when mode = "inference".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    /// Prompt to send for inference health checks.
    /// Default: "ping"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
}

impl Default for ProviderHealthCheckConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: ProviderHealthCheckMode::default(),
            interval_secs: DEFAULT_PROVIDER_HEALTH_CHECK_INTERVAL_SECS,
            timeout_secs: DEFAULT_PROVIDER_HEALTH_CHECK_TIMEOUT_SECS,
            model: None,
            prompt: None,
        }
    }
}

impl ProviderHealthCheckConfig {
    /// Get the health check interval as a Duration.
    pub fn interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.interval_secs)
    }

    /// Get the health check timeout as a Duration.
    pub fn timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.timeout_secs)
    }

    /// Get the prompt for inference health checks.
    pub fn prompt(&self) -> &str {
        self.prompt
            .as_deref()
            .unwrap_or(DEFAULT_PROVIDER_HEALTH_CHECK_PROMPT)
    }

    /// Validate the configuration.
    ///
    /// Returns an error if inference mode is enabled but no model is specified.
    pub fn validate(&self) -> Result<(), String> {
        if self.enabled && self.mode == ProviderHealthCheckMode::Inference && self.model.is_none() {
            return Err("health_check.model is required when mode = \"inference\"".into());
        }
        Ok(())
    }
}

/// Failure mode configuration for test providers.
///
/// Allows simulating various failure conditions for testing fallback behavior,
/// circuit breakers, and error handling.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TestFailureMode {
    /// Normal operation - return successful responses (default).
    #[default]
    None,

    /// Return an HTTP error status code.
    /// Use this to test retry and fallback behavior.
    HttpError {
        /// HTTP status code to return (e.g., 500, 502, 503, 504 for server errors,
        /// 400, 401, 403, 404 for client errors, 429 for rate limiting).
        status_code: u16,
        /// Optional error message to include in the response.
        #[serde(default)]
        message: Option<String>,
    },

    /// Simulate a connection/request error.
    /// Useful for testing network failure handling.
    ConnectionError {
        /// Error message describing the connection failure.
        #[serde(default = "default_connection_error_message")]
        message: String,
    },

    /// Simulate a timeout.
    /// Waits for the specified duration before returning an error.
    Timeout {
        /// Delay in milliseconds before timing out.
        #[serde(default = "default_timeout_delay_ms")]
        delay_ms: u64,
    },

    /// Fail after N successful requests (for testing circuit breaker).
    /// Alternates between success and failure based on the counter.
    FailAfterN {
        /// Number of successful requests before starting to fail.
        success_count: u32,
        /// HTTP status code to return when failing.
        #[serde(default = "default_failure_status")]
        failure_status: u16,
    },
}

fn default_connection_error_message() -> String {
    "Connection refused".to_string()
}

fn default_timeout_delay_ms() -> u64 {
    5000
}

fn default_failure_status() -> u16 {
    500
}

/// Test provider configuration.
///
/// A mock provider that returns generic responses without making real API calls.
/// Useful for testing the gateway without external dependencies.
///
/// # Failure Simulation
///
/// The `failure_mode` field allows simulating various failure conditions:
///
/// ```toml
/// # Always return 503 Service Unavailable
/// [providers.failing-provider]
/// type = "test"
/// failure_mode = { type = "http_error", status_code = 503 }
///
/// # Simulate connection errors
/// [providers.network-failure]
/// type = "test"
/// failure_mode = { type = "connection_error", message = "Connection refused" }
///
/// # Simulate timeout
/// [providers.slow-provider]
/// type = "test"
/// failure_mode = { type = "timeout", delay_ms = 5000 }
///
/// # Fail after 3 successful requests (circuit breaker testing)
/// [providers.intermittent]
/// type = "test"
/// failure_mode = { type = "fail_after_n", success_count = 3, failure_status = 500 }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct TestProviderConfig {
    /// Model name to use in responses.
    #[serde(default = "default_test_model")]
    pub model_name: String,

    /// Failure mode for simulating errors.
    /// Defaults to `none` (normal operation).
    #[serde(default)]
    pub failure_mode: TestFailureMode,

    /// Request timeout in seconds (ignored, but kept for consistency).
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,

    /// Models available through this provider.
    /// If empty, all models are allowed.
    #[serde(default)]
    pub allowed_models: Vec<String>,

    /// Model aliases.
    #[serde(default)]
    pub model_aliases: HashMap<String, String>,

    /// Per-model configuration (pricing, modalities, tasks, metadata).
    #[serde(default)]
    pub models: HashMap<String, ModelConfig>,

    /// Retry configuration for transient failures.
    #[serde(default)]
    pub retry: RetryConfig,

    /// Circuit breaker configuration for unhealthy provider protection.
    #[serde(default)]
    pub circuit_breaker: CircuitBreakerConfig,

    /// Fallback providers to try when this provider fails.
    #[serde(default)]
    pub fallback_providers: Vec<String>,

    /// Model-specific fallback configurations.
    #[serde(default)]
    pub model_fallbacks: HashMap<String, Vec<ModelFallback>>,

    /// Health check configuration for proactive provider monitoring.
    #[serde(default)]
    pub health_check: ProviderHealthCheckConfig,

    /// Override the catalog provider ID for model enrichment.
    /// Test providers typically don't need catalog enrichment.
    #[serde(default)]
    pub catalog_provider: Option<String>,

    /// Sovereignty and compliance metadata for this provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sovereignty: Option<SovereigntyMetadata>,
}

impl TestProviderConfig {
    fn validate(&self) -> Result<(), String> {
        // Test provider always validates successfully
        Ok(())
    }
}

fn default_test_model() -> String {
    "test-model".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_openai_provider() {
        let config: ProvidersConfig = toml::from_str(
            r#"
            [my-openai]
            type = "open_ai"
            api_key = "sk-test"
        "#,
        )
        .unwrap();

        assert!(config.providers.contains_key("my-openai"));
        match config.get("my-openai").unwrap() {
            ProviderConfig::OpenAi(c) => {
                assert_eq!(c.api_key, Some("sk-test".to_string()));
            }
            _ => panic!("Expected OpenAi provider"),
        }
    }

    #[test]
    fn test_parse_openrouter() {
        let config: ProvidersConfig = toml::from_str(
            r#"
            default_provider = "openrouter"

            [openrouter]
            type = "open_ai"
            api_key = "sk-or-xxx"
            base_url = "https://openrouter.ai/api/v1/"
            headers = { "HTTP-Referer" = "https://myapp.com" }
        "#,
        )
        .unwrap();

        assert_eq!(config.default_provider, Some("openrouter".to_string()));
        match config.get("openrouter").unwrap() {
            ProviderConfig::OpenAi(c) => {
                assert_eq!(c.base_url, "https://openrouter.ai/api/v1/");
            }
            _ => panic!("Expected OpenAi provider"),
        }
    }

    #[test]
    fn test_parse_anthropic_provider() {
        let config: ProvidersConfig = toml::from_str(
            r#"
            [claude]
            type = "anthropic"
            api_key = "sk-ant-xxx"
            default_max_tokens = 4096
        "#,
        )
        .unwrap();

        match config.get("claude").unwrap() {
            ProviderConfig::Anthropic(c) => {
                assert_eq!(c.api_key, "sk-ant-xxx");
                assert_eq!(c.default_max_tokens, Some(4096));
            }
            _ => panic!("Expected Anthropic provider"),
        }
    }

    #[cfg(feature = "provider-bedrock")]
    #[test]
    fn test_parse_bedrock_provider() {
        let config: ProvidersConfig = toml::from_str(
            r#"
            [aws-claude]
            type = "bedrock"
            region = "us-east-1"

            [aws-claude.credentials]
            type = "assume_role"
            role_arn = "arn:aws:iam::123456789:role/bedrock-access"
        "#,
        )
        .unwrap();

        match config.get("aws-claude").unwrap() {
            ProviderConfig::Bedrock(c) => {
                assert_eq!(c.region, "us-east-1");
                match &c.credentials {
                    AwsCredentials::AssumeRole { role_arn, .. } => {
                        assert!(role_arn.contains("bedrock-access"));
                    }
                    _ => panic!("Expected AssumeRole credentials"),
                }
            }
            _ => panic!("Expected Bedrock provider"),
        }
    }

    #[test]
    fn test_parse_local_ollama() {
        let config: ProvidersConfig = toml::from_str(
            r#"
            [local]
            type = "open_ai"
            base_url = "http://localhost:11434/v1"
            supports_tools = true
        "#,
        )
        .unwrap();

        match config.get("local").unwrap() {
            ProviderConfig::OpenAi(c) => {
                assert_eq!(c.api_key, None);
                assert_eq!(c.base_url, "http://localhost:11434/v1");
                assert!(c.supports_tools);
            }
            _ => panic!("Expected OpenAi provider"),
        }
    }

    #[cfg(feature = "provider-azure")]
    #[test]
    fn test_parse_azure_openai() {
        let config: ProvidersConfig = toml::from_str(
            r#"
            [azure-prod]
            type = "azure_open_ai"
            resource_name = "my-resource"

            [azure-prod.auth]
            type = "api_key"
            api_key = "xxx"

            [azure-prod.deployments.gpt4-deployment]
            model = "gpt-4"
            default = true
        "#,
        )
        .unwrap();

        match config.get("azure-prod").unwrap() {
            ProviderConfig::AzureOpenAi(c) => {
                assert_eq!(c.resource_name, "my-resource");
                assert!(c.deployments.contains_key("gpt4-deployment"));
            }
            _ => panic!("Expected AzureOpenAi provider"),
        }
    }

    #[test]
    fn test_multiple_providers() {
        let config: ProvidersConfig = toml::from_str(
            r#"
            default_provider = "openrouter"

            [openrouter]
            type = "open_ai"
            api_key = "sk-or-xxx"
            base_url = "https://openrouter.ai/api/v1/"

            [anthropic-direct]
            type = "anthropic"
            api_key = "sk-ant-xxx"

            [local-ollama]
            type = "open_ai"
            base_url = "http://localhost:11434/v1"
        "#,
        )
        .unwrap();

        assert_eq!(config.providers.len(), 3);
        assert_eq!(config.default_provider, Some("openrouter".to_string()));

        // Test providers_of_type helper
        let openai_providers = config.providers_of_type(ProviderType::OpenAi);
        assert_eq!(openai_providers.len(), 2);
    }

    #[test]
    fn test_model_aliases() {
        let config: ProvidersConfig = toml::from_str(
            r#"
            [openai]
            type = "open_ai"
            api_key = "sk-test"
            
            [openai.model_aliases]
            gpt4 = "gpt-4-turbo-preview"
            claude = "gpt-4"  # for people who forget which provider they're using
        "#,
        )
        .unwrap();

        let provider = config.get("openai").unwrap();
        assert_eq!(provider.resolve_model("gpt4"), "gpt-4-turbo-preview");
        assert_eq!(provider.resolve_model("gpt-4"), "gpt-4"); // no alias
    }

    #[test]
    fn test_validation_default_provider() {
        let config: ProvidersConfig = toml::from_str(
            r#"
            default_provider = "nonexistent"

            [openai]
            type = "open_ai"
            api_key = "sk-test"
        "#,
        )
        .unwrap();

        assert!(config.validate().is_err());
    }

    #[test]
    fn test_validation_missing_api_key() {
        let config: ProvidersConfig = toml::from_str(
            r#"
            [openai]
            type = "open_ai"
            # No api_key, using default OpenAI URL - should fail
        "#,
        )
        .unwrap();

        assert!(config.validate().is_err());
    }

    #[test]
    fn test_parse_model_config() {
        let config: ProvidersConfig = toml::from_str(
            r#"
            [openai]
            type = "open_ai"
            api_key = "sk-test"

            [openai.models."dall-e-3"]
            per_image = 40000
            modalities = { input = ["text"], output = ["image"] }
            tasks = ["image_generation"]
            family = "dall-e"

            [openai.models."tts-1"]
            per_1m_characters = 15000000
            modalities = { input = ["text"], output = ["audio"] }
            tasks = ["tts"]

            [openai.models."whisper-1"]
            per_second = 100
            modalities = { input = ["audio"], output = ["text"] }
            tasks = ["transcription", "translation"]
            family = "whisper"

            [openai.models."gpt-4o"]
            input_per_1m_tokens = 2500000
            output_per_1m_tokens = 10000000
        "#,
        )
        .unwrap();

        let pc = config.get("openai").unwrap();

        // DALL-E 3: image generation with per_image pricing
        let dalle = pc.get_model_config("dall-e-3").unwrap();
        assert_eq!(dalle.pricing.per_image, Some(40000));
        assert_eq!(dalle.tasks, vec!["image_generation"]);
        assert_eq!(dalle.family, Some("dall-e".to_string()));
        let mods = dalle.modalities.as_ref().unwrap();
        assert_eq!(mods.output, vec!["image"]);

        // TTS: character pricing
        let tts = pc.get_model_config("tts-1").unwrap();
        assert_eq!(tts.pricing.per_1m_characters, Some(15_000_000));
        assert_eq!(tts.tasks, vec!["tts"]);

        // Whisper: per-second pricing, dual tasks
        let whisper = pc.get_model_config("whisper-1").unwrap();
        assert_eq!(whisper.pricing.per_second, Some(100));
        assert_eq!(whisper.tasks, vec!["transcription", "translation"]);

        // GPT-4o: token pricing only (no extra metadata)
        let gpt4o = pc.get_model_config("gpt-4o").unwrap();
        assert_eq!(gpt4o.pricing.input_per_1m_tokens, 2_500_000);
        assert_eq!(gpt4o.pricing.output_per_1m_tokens, 10_000_000);
        assert!(gpt4o.tasks.is_empty());
        assert!(gpt4o.modalities.is_none());

        // get_pricing accessor should extract pricing from ModelConfig
        let pricing = pc.get_pricing("dall-e-3").unwrap();
        assert_eq!(pricing.per_image, Some(40000));
    }

    #[test]
    fn test_parse_test_provider() {
        let config: ProvidersConfig = toml::from_str(
            r#"
            default_provider = "test"

            [test]
            type = "test"
            model_name = "test-model"
        "#,
        )
        .unwrap();

        assert_eq!(config.default_provider, Some("test".to_string()));
        match config.get("test").unwrap() {
            ProviderConfig::Test(c) => {
                assert_eq!(c.model_name, "test-model");
                assert_eq!(c.timeout_secs, 300); // default
            }
            _ => panic!("Expected Test provider"),
        }

        // Validate should succeed
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_parse_test_provider_minimal() {
        let config: ProvidersConfig = toml::from_str(
            r#"
            [mock]
            type = "test"
        "#,
        )
        .unwrap();

        match config.get("mock").unwrap() {
            ProviderConfig::Test(c) => {
                assert_eq!(c.model_name, "test-model"); // default
            }
            _ => panic!("Expected Test provider"),
        }
    }

    #[test]
    fn test_parse_fallback_providers() {
        let config: ProvidersConfig = toml::from_str(
            r#"
            [primary-openai]
            type = "open_ai"
            api_key = "sk-xxx"
            fallback_providers = ["backup-anthropic", "local-ollama"]

            [backup-anthropic]
            type = "anthropic"
            api_key = "sk-ant-xxx"

            [local-ollama]
            type = "open_ai"
            base_url = "http://localhost:11434/v1"
        "#,
        )
        .unwrap();

        let provider = config.get("primary-openai").unwrap();
        assert_eq!(
            provider.fallback_providers(),
            &["backup-anthropic".to_string(), "local-ollama".to_string()]
        );
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_parse_model_fallbacks() {
        let config: ProvidersConfig = toml::from_str(
            r#"
            [primary-openai]
            type = "open_ai"
            api_key = "sk-xxx"

            [primary-openai.model_fallbacks]
            "gpt-4o" = [
                { model = "gpt-4o-mini" },
                { model = "gpt-4-turbo" },
                { provider = "backup-anthropic", model = "claude-sonnet-4-20250514" }
            ]

            [backup-anthropic]
            type = "anthropic"
            api_key = "sk-ant-xxx"
        "#,
        )
        .unwrap();

        let provider = config.get("primary-openai").unwrap();
        let fallbacks = provider.get_model_fallbacks("gpt-4o").unwrap();
        assert_eq!(fallbacks.len(), 3);
        assert_eq!(fallbacks[0].model, "gpt-4o-mini");
        assert_eq!(fallbacks[0].provider, None);
        assert_eq!(fallbacks[1].model, "gpt-4-turbo");
        assert_eq!(fallbacks[2].model, "claude-sonnet-4-20250514");
        assert_eq!(fallbacks[2].provider, Some("backup-anthropic".to_string()));
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_validation_fallback_provider_not_found() {
        let config: ProvidersConfig = toml::from_str(
            r#"
            [primary-openai]
            type = "open_ai"
            api_key = "sk-xxx"
            fallback_providers = ["nonexistent"]
        "#,
        )
        .unwrap();

        let err = config.validate().unwrap_err();
        assert!(
            err.to_string()
                .contains("fallback provider 'nonexistent' is not defined")
        );
    }

    #[test]
    fn test_validation_fallback_provider_self_reference() {
        let config: ProvidersConfig = toml::from_str(
            r#"
            [primary-openai]
            type = "open_ai"
            api_key = "sk-xxx"
            fallback_providers = ["primary-openai"]
        "#,
        )
        .unwrap();

        let err = config.validate().unwrap_err();
        assert!(
            err.to_string()
                .contains("cannot use self as fallback provider")
        );
    }

    #[test]
    fn test_validation_model_fallback_provider_not_found() {
        let config: ProvidersConfig = toml::from_str(
            r#"
            [primary-openai]
            type = "open_ai"
            api_key = "sk-xxx"

            [primary-openai.model_fallbacks]
            "gpt-4o" = [
                { provider = "nonexistent", model = "some-model" }
            ]
        "#,
        )
        .unwrap();

        let err = config.validate().unwrap_err();
        assert!(
            err.to_string()
                .contains("model_fallbacks['gpt-4o'][0].provider 'nonexistent' is not defined")
        );
    }

    #[test]
    fn test_combined_fallback_config() {
        // Test both provider-level and model-level fallbacks together
        let config: ProvidersConfig = toml::from_str(
            r#"
            [primary-openai]
            type = "open_ai"
            api_key = "sk-xxx"
            fallback_providers = ["backup-anthropic"]

            [primary-openai.model_fallbacks]
            "gpt-4o" = [
                { model = "gpt-4o-mini" },
                { provider = "backup-anthropic", model = "claude-sonnet-4-20250514" }
            ]
            "gpt-4o-mini" = [
                { model = "gpt-3.5-turbo" }
            ]

            [backup-anthropic]
            type = "anthropic"
            api_key = "sk-ant-xxx"

            [backup-anthropic.model_fallbacks]
            "claude-sonnet-4-20250514" = [
                { model = "claude-3-5-sonnet-20241022" },
                { model = "claude-3-5-haiku-20241022" }
            ]
        "#,
        )
        .unwrap();

        assert!(config.validate().is_ok());

        let primary = config.get("primary-openai").unwrap();
        assert_eq!(
            primary.fallback_providers(),
            &["backup-anthropic".to_string()]
        );
        assert_eq!(primary.model_fallbacks().len(), 2);

        let backup = config.get("backup-anthropic").unwrap();
        assert!(backup.fallback_providers().is_empty());
        assert_eq!(backup.model_fallbacks().len(), 1);
    }

    #[test]
    fn test_parse_test_provider_with_http_error_failure_mode() {
        let config: ProvidersConfig = toml::from_str(
            r#"
            [failing-provider]
            type = "test"
            failure_mode = { type = "http_error", status_code = 503, message = "Service Unavailable" }
        "#,
        )
        .unwrap();

        match config.get("failing-provider").unwrap() {
            ProviderConfig::Test(c) => {
                assert_eq!(
                    c.failure_mode,
                    TestFailureMode::HttpError {
                        status_code: 503,
                        message: Some("Service Unavailable".to_string())
                    }
                );
            }
            _ => panic!("Expected Test provider"),
        }
    }

    #[test]
    fn test_parse_test_provider_with_connection_error() {
        let config: ProvidersConfig = toml::from_str(
            r#"
            [network-failure]
            type = "test"
            failure_mode = { type = "connection_error", message = "Host unreachable" }
        "#,
        )
        .unwrap();

        match config.get("network-failure").unwrap() {
            ProviderConfig::Test(c) => {
                assert_eq!(
                    c.failure_mode,
                    TestFailureMode::ConnectionError {
                        message: "Host unreachable".to_string()
                    }
                );
            }
            _ => panic!("Expected Test provider"),
        }
    }

    #[test]
    fn test_parse_test_provider_with_timeout() {
        let config: ProvidersConfig = toml::from_str(
            r#"
            [slow-provider]
            type = "test"
            failure_mode = { type = "timeout", delay_ms = 10000 }
        "#,
        )
        .unwrap();

        match config.get("slow-provider").unwrap() {
            ProviderConfig::Test(c) => {
                assert_eq!(c.failure_mode, TestFailureMode::Timeout { delay_ms: 10000 });
            }
            _ => panic!("Expected Test provider"),
        }
    }

    #[test]
    fn test_parse_test_provider_with_fail_after_n() {
        let config: ProvidersConfig = toml::from_str(
            r#"
            [intermittent]
            type = "test"
            failure_mode = { type = "fail_after_n", success_count = 3, failure_status = 502 }
        "#,
        )
        .unwrap();

        match config.get("intermittent").unwrap() {
            ProviderConfig::Test(c) => {
                assert_eq!(
                    c.failure_mode,
                    TestFailureMode::FailAfterN {
                        success_count: 3,
                        failure_status: 502
                    }
                );
            }
            _ => panic!("Expected Test provider"),
        }
    }

    #[test]
    fn test_parse_test_provider_default_failure_mode() {
        let config: ProvidersConfig = toml::from_str(
            r#"
            [normal]
            type = "test"
        "#,
        )
        .unwrap();

        match config.get("normal").unwrap() {
            ProviderConfig::Test(c) => {
                assert_eq!(c.failure_mode, TestFailureMode::None);
            }
            _ => panic!("Expected Test provider"),
        }
    }

    #[test]
    fn test_parse_test_provider_with_fallback_and_failure_mode() {
        let config: ProvidersConfig = toml::from_str(
            r#"
            [primary]
            type = "test"
            failure_mode = { type = "http_error", status_code = 500 }
            fallback_providers = ["backup"]

            [backup]
            type = "test"
            # No failure_mode - should succeed
        "#,
        )
        .unwrap();

        assert!(config.validate().is_ok());

        let primary = config.get("primary").unwrap();
        assert_eq!(primary.fallback_providers(), &["backup".to_string()]);
        match primary {
            ProviderConfig::Test(c) => {
                assert_eq!(
                    c.failure_mode,
                    TestFailureMode::HttpError {
                        status_code: 500,
                        message: None
                    }
                );
            }
            _ => panic!("Expected Test provider"),
        }

        let backup = config.get("backup").unwrap();
        match backup {
            ProviderConfig::Test(c) => {
                assert_eq!(c.failure_mode, TestFailureMode::None);
            }
            _ => panic!("Expected Test provider"),
        }
    }

    // Tests for operation-specific retry config methods

    #[test]
    fn test_retry_config_for_embedding() {
        let config = RetryConfig::default();
        let embedding_config = config.for_embedding();

        // Embeddings should use more retries (idempotent operation)
        assert_eq!(embedding_config.max_retries, EMBEDDING_MAX_RETRIES);
        assert_eq!(embedding_config.max_retries, 5);

        // Other settings should be preserved
        assert_eq!(embedding_config.enabled, config.enabled);
        assert_eq!(embedding_config.initial_delay_ms, config.initial_delay_ms);
        assert_eq!(embedding_config.max_delay_ms, config.max_delay_ms);
        assert_eq!(
            embedding_config.backoff_multiplier,
            config.backoff_multiplier
        );
    }

    #[test]
    fn test_retry_config_for_image_generation() {
        let config = RetryConfig::default();
        let image_config = config.for_image_generation();

        // Image generation should use fewer retries (not idempotent, creates duplicates)
        assert_eq!(image_config.max_retries, IMAGE_GENERATION_MAX_RETRIES);
        assert_eq!(image_config.max_retries, 1);

        // Other settings should be preserved
        assert_eq!(image_config.enabled, config.enabled);
        assert_eq!(image_config.initial_delay_ms, config.initial_delay_ms);
    }

    #[test]
    fn test_retry_config_for_read_only() {
        let config = RetryConfig::default();
        let read_only_config = config.for_read_only();

        // Read-only operations should use more retries (no side effects)
        assert_eq!(read_only_config.max_retries, READ_ONLY_MAX_RETRIES);
        assert_eq!(read_only_config.max_retries, 5);

        // Other settings should be preserved
        assert_eq!(read_only_config.enabled, config.enabled);
    }

    #[test]
    fn test_retry_config_cow_borrowed_when_unchanged() {
        // When max_retries already matches the target, should return Cow::Borrowed
        let config = RetryConfig {
            max_retries: EMBEDDING_MAX_RETRIES,
            ..Default::default()
        };
        let embedding_config = config.for_embedding();

        // Should be borrowed (no allocation) since max_retries already matches
        assert!(matches!(embedding_config, std::borrow::Cow::Borrowed(_)));
    }

    #[test]
    fn test_retry_config_cow_owned_when_changed() {
        // When max_retries differs from target, should return Cow::Owned
        let config = RetryConfig::default(); // max_retries = 3
        let embedding_config = config.for_embedding(); // target = 5

        // Should be owned (new allocation) since max_retries changed
        assert!(matches!(embedding_config, std::borrow::Cow::Owned(_)));
    }

    #[test]
    fn test_retry_config_custom_embedding_retries() {
        let config = RetryConfig {
            embedding_max_retries: Some(10),
            ..Default::default()
        };
        let embedding_config = config.for_embedding();

        // Should use the custom configured value
        assert_eq!(embedding_config.max_retries, 10);
    }

    #[test]
    fn test_retry_config_custom_image_generation_retries() {
        let config = RetryConfig {
            image_generation_max_retries: Some(0), // Disable retries entirely
            ..Default::default()
        };
        let image_config = config.for_image_generation();

        // Should use the custom configured value
        assert_eq!(image_config.max_retries, 0);
    }

    #[test]
    fn test_retry_config_custom_read_only_retries() {
        let config = RetryConfig {
            read_only_max_retries: Some(7),
            ..Default::default()
        };
        let read_only_config = config.for_read_only();

        // Should use the custom configured value
        assert_eq!(read_only_config.max_retries, 7);
    }

    #[test]
    fn test_parse_retry_config_with_operation_overrides() {
        let config: ProvidersConfig = toml::from_str(
            r#"
            [my-openai]
            type = "open_ai"
            api_key = "sk-test"

            [my-openai.retry]
            max_retries = 3
            embedding_max_retries = 10
            image_generation_max_retries = 0
            read_only_max_retries = 8
        "#,
        )
        .unwrap();

        match config.get("my-openai").unwrap() {
            ProviderConfig::OpenAi(c) => {
                assert_eq!(c.retry.max_retries, 3);
                assert_eq!(c.retry.embedding_max_retries, Some(10));
                assert_eq!(c.retry.image_generation_max_retries, Some(0));
                assert_eq!(c.retry.read_only_max_retries, Some(8));

                // Verify the for_* methods use the configured values
                assert_eq!(c.retry.for_embedding().max_retries, 10);
                assert_eq!(c.retry.for_image_generation().max_retries, 0);
                assert_eq!(c.retry.for_read_only().max_retries, 8);
            }
            _ => panic!("Expected OpenAi provider"),
        }
    }

    #[cfg(all(feature = "provider-bedrock", feature = "provider-vertex"))]
    #[test]
    fn test_streaming_buffer_config_accessor() {
        // Providers that transform streams should return Some
        let config: ProvidersConfig = toml::from_str(
            r#"
            [anthropic]
            type = "anthropic"
            api_key = "sk-ant-xxx"

            [anthropic.streaming_buffer]
            max_input_buffer_bytes = 8388608
            max_output_buffer_chunks = 500

            [bedrock]
            type = "bedrock"
            region = "us-east-1"

            [vertex]
            type = "vertex"
            api_key = "test-key"
        "#,
        )
        .unwrap();

        // Anthropic should have streaming buffer config
        let anthropic = config.get("anthropic").unwrap();
        let streaming_buffer = anthropic.streaming_buffer_config();
        assert!(streaming_buffer.is_some());
        let sb = streaming_buffer.unwrap();
        assert_eq!(sb.max_input_buffer_bytes, 8388608);
        assert_eq!(sb.max_output_buffer_chunks, 500);

        // Bedrock should have streaming buffer config (with defaults)
        let bedrock = config.get("bedrock").unwrap();
        assert!(bedrock.streaming_buffer_config().is_some());

        // Vertex should have streaming buffer config (with defaults)
        let vertex = config.get("vertex").unwrap();
        assert!(vertex.streaming_buffer_config().is_some());
    }

    #[cfg(feature = "provider-azure")]
    #[test]
    fn test_streaming_buffer_config_none_for_passthrough_providers() {
        // Providers that pass through streams should return None
        let config: ProvidersConfig = toml::from_str(
            r#"
            [openai]
            type = "open_ai"
            api_key = "sk-xxx"

            [azure]
            type = "azure_open_ai"
            resource_name = "my-resource"
            [azure.auth]
            type = "api_key"
            api_key = "xxx"

            [test]
            type = "test"
        "#,
        )
        .unwrap();

        // OpenAI passes through streams - no buffer config
        let openai = config.get("openai").unwrap();
        assert!(openai.streaming_buffer_config().is_none());

        // Azure OpenAI passes through streams - no buffer config
        let azure = config.get("azure").unwrap();
        assert!(azure.streaming_buffer_config().is_none());

        // Test provider - no buffer config
        let test = config.get("test").unwrap();
        assert!(test.streaming_buffer_config().is_none());
    }

    // ==================== Credential Redaction Tests ====================
    // These tests verify that sensitive credentials are NOT exposed in Debug output.
    // This prevents accidental credential leakage in logs, panic messages, or error output.

    #[test]
    fn test_openai_config_debug_redacts_api_key() {
        let config = OpenAiProviderConfig {
            api_key: Some("sk-secret-key-12345".to_string()),
            base_url: "https://api.openai.com/v1".to_string(),
            organization: None,
            project: None,
            timeout_secs: 300,
            allowed_models: vec![],
            model_aliases: HashMap::new(),
            headers: HashMap::new(),
            supports_tools: false,
            supports_vision: false,
            models: HashMap::new(),
            retry: RetryConfig::default(),
            circuit_breaker: CircuitBreakerConfig::default(),
            fallback_providers: vec![],
            model_fallbacks: HashMap::new(),
            health_check: ProviderHealthCheckConfig::default(),
            catalog_provider: None,
            sovereignty: None,
        };

        let debug_output = format!("{:?}", config);
        assert!(
            debug_output.contains("****"),
            "Debug output should contain redacted marker"
        );
        assert!(
            !debug_output.contains("sk-secret-key-12345"),
            "Debug output must NOT contain actual API key"
        );
    }

    #[test]
    fn test_anthropic_config_debug_redacts_api_key() {
        let config = AnthropicProviderConfig {
            api_key: "sk-ant-secret-key-67890".to_string(),
            base_url: "https://api.anthropic.com".to_string(),
            timeout_secs: 300,
            default_model: None,
            default_max_tokens: None,
            allowed_models: vec![],
            model_aliases: HashMap::new(),
            models: HashMap::new(),
            retry: RetryConfig::default(),
            circuit_breaker: CircuitBreakerConfig::default(),
            streaming_buffer: StreamingBufferConfig::default(),
            fallback_providers: vec![],
            model_fallbacks: HashMap::new(),
            health_check: ProviderHealthCheckConfig::default(),
            catalog_provider: None,
            sovereignty: None,
            interleaved_thinking_models: default_interleaved_thinking_models(),
            adaptive_thinking_models: default_adaptive_thinking_models(),
            strict_thinking_models: default_strict_thinking_models(),
            mid_conversation_system_models: default_mid_conversation_system_models(),
        };

        let debug_output = format!("{:?}", config);
        assert!(
            debug_output.contains("****"),
            "Debug output should contain redacted marker"
        );
        assert!(
            !debug_output.contains("sk-ant-secret-key-67890"),
            "Debug output must NOT contain actual API key"
        );
    }

    #[cfg(feature = "provider-bedrock")]
    #[test]
    fn test_aws_credentials_static_debug_redacts_secrets() {
        let creds = AwsCredentials::Static {
            access_key_id: "AKIAIOSFODNN7EXAMPLE".to_string(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_string(),
            session_token: Some("FwoGZXIvYXdzEBYaD...session-token".to_string()),
        };

        let debug_output = format!("{:?}", creds);
        assert!(
            debug_output.contains("****"),
            "Debug output should contain redacted marker"
        );
        assert!(
            !debug_output.contains("AKIAIOSFODNN7EXAMPLE"),
            "Debug output must NOT contain access key ID"
        );
        assert!(
            !debug_output.contains("wJalrXUtnFEMI"),
            "Debug output must NOT contain secret access key"
        );
        assert!(
            !debug_output.contains("session-token"),
            "Debug output must NOT contain session token"
        );
    }

    #[cfg(feature = "provider-bedrock")]
    #[test]
    fn test_aws_credentials_non_static_not_redacted() {
        // Non-sensitive variants should display normally
        let creds = AwsCredentials::Profile {
            name: "production".to_string(),
        };
        let debug_output = format!("{:?}", creds);
        assert!(
            debug_output.contains("production"),
            "Profile name should be visible"
        );

        let creds = AwsCredentials::AssumeRole {
            role_arn: "arn:aws:iam::123456789:role/MyRole".to_string(),
            external_id: Some("external-123".to_string()),
            session_name: None,
        };
        let debug_output = format!("{:?}", creds);
        assert!(
            debug_output.contains("arn:aws:iam"),
            "Role ARN should be visible"
        );
    }

    #[cfg(feature = "provider-vertex")]
    #[test]
    fn test_vertex_config_debug_redacts_api_key() {
        let config = VertexProviderConfig {
            api_key: Some("AIzaSy-google-api-key-secret".to_string()),
            project: Some("my-project".to_string()),
            region: Some("us-central1".to_string()),
            publisher: "google".to_string(),
            base_url: None,
            credentials: GcpCredentials::Default,
            timeout_secs: 300,
            allowed_models: vec![],
            model_aliases: HashMap::new(),
            models: HashMap::new(),
            retry: RetryConfig::default(),
            circuit_breaker: CircuitBreakerConfig::default(),
            streaming_buffer: StreamingBufferConfig::default(),
            fallback_providers: vec![],
            model_fallbacks: HashMap::new(),
            health_check: ProviderHealthCheckConfig::default(),
            catalog_provider: None,
            sovereignty: None,
        };

        let debug_output = format!("{:?}", config);
        assert!(
            debug_output.contains("****"),
            "Debug output should contain redacted marker"
        );
        assert!(
            !debug_output.contains("AIzaSy-google-api-key-secret"),
            "Debug output must NOT contain actual API key"
        );
        // Non-sensitive fields should still be visible
        assert!(
            debug_output.contains("my-project"),
            "Project should be visible"
        );
    }

    #[cfg(feature = "provider-azure")]
    #[test]
    fn test_azure_auth_api_key_debug_redacts_key() {
        let auth = AzureAuth::ApiKey {
            api_key: "azure-api-key-secret-12345".to_string(),
        };

        let debug_output = format!("{:?}", auth);
        assert!(
            debug_output.contains("****"),
            "Debug output should contain redacted marker"
        );
        assert!(
            !debug_output.contains("azure-api-key-secret-12345"),
            "Debug output must NOT contain actual API key"
        );
    }

    #[cfg(feature = "provider-azure")]
    #[test]
    fn test_azure_auth_azure_ad_debug_redacts_secret() {
        let auth = AzureAuth::AzureAd {
            tenant_id: "tenant-id-visible".to_string(),
            client_id: "client-id-visible".to_string(),
            client_secret: "super-secret-client-secret".to_string(),
        };

        let debug_output = format!("{:?}", auth);
        assert!(
            debug_output.contains("****"),
            "Debug output should contain redacted marker"
        );
        assert!(
            !debug_output.contains("super-secret-client-secret"),
            "Debug output must NOT contain client secret"
        );
        // Non-sensitive fields should still be visible
        assert!(
            debug_output.contains("tenant-id-visible"),
            "Tenant ID should be visible"
        );
        assert!(
            debug_output.contains("client-id-visible"),
            "Client ID should be visible"
        );
    }

    #[cfg(feature = "provider-azure")]
    #[test]
    fn test_azure_auth_managed_identity_not_redacted() {
        // Managed identity has no secrets to redact
        let auth = AzureAuth::ManagedIdentity {
            client_id: Some("mi-client-id".to_string()),
        };

        let debug_output = format!("{:?}", auth);
        assert!(
            debug_output.contains("mi-client-id"),
            "Client ID should be visible for managed identity"
        );
    }

    // ============================================================================
    // Health Check Config Tests
    // ============================================================================

    #[test]
    fn test_parse_health_check_reachability_mode() {
        let config: ProvidersConfig = toml::from_str(
            r#"
            [my-openai]
            type = "open_ai"
            api_key = "sk-test"

            [my-openai.health_check]
            enabled = true
            mode = "reachability"
            interval_secs = 30
            timeout_secs = 5
        "#,
        )
        .unwrap();

        match config.get("my-openai").unwrap() {
            ProviderConfig::OpenAi(c) => {
                assert!(c.health_check.enabled);
                assert_eq!(c.health_check.mode, ProviderHealthCheckMode::Reachability);
                assert_eq!(c.health_check.interval_secs, 30);
                assert_eq!(c.health_check.timeout_secs, 5);
                assert!(c.health_check.model.is_none());
            }
            _ => panic!("Expected OpenAi provider"),
        }
    }

    #[test]
    fn test_parse_health_check_inference_mode() {
        let config: ProvidersConfig = toml::from_str(
            r#"
            [my-openai]
            type = "open_ai"
            api_key = "sk-test"

            [my-openai.health_check]
            enabled = true
            mode = "inference"
            model = "gpt-4o-mini"
            prompt = "Say OK"
            interval_secs = 120
        "#,
        )
        .unwrap();

        match config.get("my-openai").unwrap() {
            ProviderConfig::OpenAi(c) => {
                assert!(c.health_check.enabled);
                assert_eq!(c.health_check.mode, ProviderHealthCheckMode::Inference);
                assert_eq!(c.health_check.model, Some("gpt-4o-mini".to_string()));
                assert_eq!(c.health_check.prompt, Some("Say OK".to_string()));
                assert_eq!(c.health_check.interval_secs, 120);
            }
            _ => panic!("Expected OpenAi provider"),
        }
    }

    #[test]
    fn test_parse_health_check_defaults() {
        let config: ProvidersConfig = toml::from_str(
            r#"
            [my-openai]
            type = "open_ai"
            api_key = "sk-test"

            [my-openai.health_check]
            enabled = true
        "#,
        )
        .unwrap();

        match config.get("my-openai").unwrap() {
            ProviderConfig::OpenAi(c) => {
                assert!(c.health_check.enabled);
                // Defaults should be applied
                assert_eq!(c.health_check.mode, ProviderHealthCheckMode::Reachability);
                assert_eq!(
                    c.health_check.interval_secs,
                    DEFAULT_PROVIDER_HEALTH_CHECK_INTERVAL_SECS
                );
                assert_eq!(
                    c.health_check.timeout_secs,
                    DEFAULT_PROVIDER_HEALTH_CHECK_TIMEOUT_SECS
                );
            }
            _ => panic!("Expected OpenAi provider"),
        }
    }

    #[test]
    fn test_parse_health_check_disabled_by_default() {
        let config: ProvidersConfig = toml::from_str(
            r#"
            [my-openai]
            type = "open_ai"
            api_key = "sk-test"
        "#,
        )
        .unwrap();

        match config.get("my-openai").unwrap() {
            ProviderConfig::OpenAi(c) => {
                // Health checks should be disabled by default
                assert!(!c.health_check.enabled);
            }
            _ => panic!("Expected OpenAi provider"),
        }
    }

    #[test]
    fn test_health_check_config_validate_inference_requires_model() {
        let config = ProviderHealthCheckConfig {
            enabled: true,
            mode: ProviderHealthCheckMode::Inference,
            model: None, // Missing model!
            ..Default::default()
        };

        let err = config.validate().unwrap_err();
        assert!(err.contains("model is required"));
    }

    #[test]
    fn test_health_check_config_validate_inference_with_model() {
        let config = ProviderHealthCheckConfig {
            enabled: true,
            mode: ProviderHealthCheckMode::Inference,
            model: Some("gpt-4o-mini".to_string()),
            ..Default::default()
        };

        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_health_check_config_validate_reachability_no_model_required() {
        let config = ProviderHealthCheckConfig {
            enabled: true,
            mode: ProviderHealthCheckMode::Reachability,
            model: None,
            ..Default::default()
        };

        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_health_check_config_validate_disabled_skips_checks() {
        // When disabled, validation should pass even with invalid config
        let config = ProviderHealthCheckConfig {
            enabled: false,
            mode: ProviderHealthCheckMode::Inference,
            model: None, // Would be invalid if enabled
            ..Default::default()
        };

        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_health_check_config_helper_methods() {
        let config = ProviderHealthCheckConfig {
            enabled: true,
            mode: ProviderHealthCheckMode::Inference,
            interval_secs: 60,
            timeout_secs: 10,
            model: Some("test-model".to_string()),
            prompt: Some("Hello".to_string()),
        };

        assert_eq!(config.interval(), std::time::Duration::from_secs(60));
        assert_eq!(config.timeout(), std::time::Duration::from_secs(10));
        assert_eq!(config.prompt(), "Hello");
    }

    #[test]
    fn test_health_check_config_default_prompt() {
        let config = ProviderHealthCheckConfig::default();
        // Should return the default prompt when none is set
        assert_eq!(config.prompt(), DEFAULT_PROVIDER_HEALTH_CHECK_PROMPT);
    }

    #[cfg(all(
        feature = "provider-bedrock",
        feature = "provider-vertex",
        feature = "provider-azure"
    ))]
    #[test]
    fn test_health_check_config_accessor_all_providers() {
        // Test that health_check_config() works for all provider types
        let config: ProvidersConfig = toml::from_str(
            r#"
            [openai-test]
            type = "open_ai"
            api_key = "sk-test"
            [openai-test.health_check]
            enabled = true

            [anthropic-test]
            type = "anthropic"
            api_key = "sk-ant-test"
            [anthropic-test.health_check]
            enabled = true
            interval_secs = 45

            [bedrock-test]
            type = "bedrock"
            region = "us-east-1"
            [bedrock-test.health_check]
            enabled = true
            mode = "inference"
            model = "anthropic.claude-3-haiku-20240307-v1:0"

            [vertex-test]
            type = "vertex"
            project = "my-project"
            region = "us-central1"
            [vertex-test.health_check]
            enabled = true
            timeout_secs = 15

            [azure-test]
            type = "azure_open_ai"
            resource_name = "my-resource"
            [azure-test.auth]
            type = "api_key"
            api_key = "xxx"
            [azure-test.health_check]
            enabled = true

            [test-provider]
            type = "test"
            [test-provider.health_check]
            enabled = true
        "#,
        )
        .unwrap();

        // OpenAI
        let openai = config.get("openai-test").unwrap();
        assert!(openai.health_check_config().enabled);

        // Anthropic
        let anthropic = config.get("anthropic-test").unwrap();
        assert!(anthropic.health_check_config().enabled);
        assert_eq!(anthropic.health_check_config().interval_secs, 45);

        // Bedrock
        let bedrock = config.get("bedrock-test").unwrap();
        assert!(bedrock.health_check_config().enabled);
        assert_eq!(
            bedrock.health_check_config().mode,
            ProviderHealthCheckMode::Inference
        );

        // Vertex
        let vertex = config.get("vertex-test").unwrap();
        assert!(vertex.health_check_config().enabled);
        assert_eq!(vertex.health_check_config().timeout_secs, 15);

        // Azure OpenAI
        let azure = config.get("azure-test").unwrap();
        assert!(azure.health_check_config().enabled);

        // Test provider
        let test = config.get("test-provider").unwrap();
        assert!(test.health_check_config().enabled);
    }
}
