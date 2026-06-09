//! Thread-safe model catalog registry for enriching API responses.
//!
//! The registry provides O(1) lookup of model metadata by provider and model ID,
//! with support for runtime updates from background sync jobs.

use std::{collections::HashMap, sync::Arc};

use serde::{Deserialize, Serialize};

use super::types::{CatalogCost, CatalogModel, ModelCatalog};
use crate::{compat::RwLock, pricing::ModelPricing};

/// Model capabilities extracted from the catalog.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct ModelCapabilities {
    /// Whether the model supports image/file attachments (vision)
    pub vision: bool,

    /// Whether the model supports reasoning/thinking mode
    pub reasoning: bool,

    /// Whether the model supports tool/function calling
    pub tool_call: bool,

    /// Whether the model supports structured output (JSON mode)
    pub structured_output: bool,

    /// Whether the model supports temperature control
    pub temperature: bool,
}

/// Model limits from the catalog.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct ModelLimits {
    /// Maximum context window size (tokens)
    pub context_length: Option<i64>,

    /// Maximum output tokens
    pub max_output_tokens: Option<i64>,
}

/// Model modalities from the catalog.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct ModelModalities {
    /// Supported input modalities (e.g., "text", "image", "audio")
    pub input: Vec<String>,

    /// Supported output modalities (e.g., "text", "audio")
    pub output: Vec<String>,
}

/// Catalog pricing in dollars per 1M tokens (for display purposes).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct CatalogPricing {
    /// Input token cost ($/1M tokens)
    pub input: f64,

    /// Output token cost ($/1M tokens)
    pub output: f64,

    /// Reasoning token cost ($/1M tokens)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<f64>,

    /// Cache read cost ($/1M tokens)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<f64>,

    /// Cache write cost ($/1M tokens)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<f64>,
}

/// Enrichment data for a model from the catalog.
#[derive(Debug, Clone, Default)]
pub struct ModelEnrichment {
    /// Model capabilities
    pub capabilities: ModelCapabilities,

    /// Context and output limits
    pub limits: ModelLimits,

    /// Pricing for internal cost calculation (in microcents)
    pub pricing: ModelPricing,

    /// Pricing for display (in dollars per 1M tokens)
    pub catalog_pricing: CatalogPricing,

    /// Input/output modalities
    pub modalities: ModelModalities,

    /// Supported tasks / API endpoints (e.g., "chat", "image_generation", "tts").
    pub tasks: Vec<String>,

    /// Model family
    pub family: Option<String>,

    /// Model release date
    pub release_date: Option<String>,

    /// Whether the model has open weights
    pub open_weights: bool,
}

/// Thread-safe registry for model catalog data.
#[derive(Clone)]
pub struct ModelCatalogRegistry {
    /// Map from (provider_id, model_id) to enrichment data
    inner: Arc<RwLock<HashMap<(String, String), ModelEnrichment>>>,
}

impl Default for ModelCatalogRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ModelCatalogRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Load catalog data from JSON string.
    ///
    /// This replaces all existing data in the registry.
    pub fn load_from_json(&self, json: &str) -> Result<(), serde_json::Error> {
        let catalog: ModelCatalog = serde_json::from_str(json)?;
        self.load_from_catalog(&catalog);
        Ok(())
    }

    /// Load catalog data from a parsed catalog.
    pub fn load_from_catalog(&self, catalog: &ModelCatalog) {
        let mut data = HashMap::new();

        for (provider_id, provider) in catalog {
            for (model_id, model) in &provider.models {
                let enrichment = Self::model_to_enrichment(model);
                data.insert((provider_id.clone(), model_id.clone()), enrichment);
            }
        }

        let mut inner = self.inner.write();
        *inner = data;
    }

    /// Look up enrichment data for a model.
    pub fn lookup(&self, provider_id: &str, model_id: &str) -> Option<ModelEnrichment> {
        let inner = self.inner.read();
        inner
            .get(&(provider_id.to_string(), model_id.to_string()))
            .cloned()
    }

    /// Get pricing for a model (for cost calculation).
    ///
    /// Optimized to only clone the `ModelPricing` instead of the full `ModelEnrichment`.
    pub fn get_pricing(&self, provider_id: &str, model_id: &str) -> Option<ModelPricing> {
        let inner = self.inner.read();
        inner
            .get(&(provider_id.to_string(), model_id.to_string()))
            .map(|e| e.pricing.clone())
    }

    /// Get the number of models in the registry.
    pub fn model_count(&self) -> usize {
        self.inner.read().len()
    }

    /// Convert a catalog model to enrichment data.
    fn model_to_enrichment(model: &CatalogModel) -> ModelEnrichment {
        ModelEnrichment {
            capabilities: ModelCapabilities {
                vision: model.attachment,
                reasoning: model.reasoning,
                tool_call: model.tool_call,
                structured_output: model.structured_output,
                temperature: model.temperature,
            },
            limits: ModelLimits {
                context_length: if model.limit.context > 0 {
                    Some(model.limit.context)
                } else {
                    None
                },
                max_output_tokens: if model.limit.output > 0 {
                    Some(model.limit.output)
                } else {
                    None
                },
            },
            pricing: catalog_cost_to_model_pricing(&model.cost),
            catalog_pricing: CatalogPricing {
                input: model.cost.input,
                output: model.cost.output,
                reasoning: model.cost.reasoning,
                cache_read: model.cost.cache_read,
                cache_write: model.cost.cache_write,
            },
            modalities: ModelModalities {
                input: model.modalities.input.clone(),
                output: model.modalities.output.clone(),
            },
            tasks: Vec::new(),
            family: model.family.clone(),
            release_date: model.release_date.clone(),
            open_weights: model.open_weights,
        }
    }
}

impl std::fmt::Debug for ModelCatalogRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let count = self.model_count();
        f.debug_struct("ModelCatalogRegistry")
            .field("model_count", &count)
            .finish()
    }
}

/// Convert catalog pricing (dollars per 1M tokens) to ModelPricing (microcents per 1M tokens).
///
/// The catalog uses dollars per 1M tokens (e.g., $5.00 per 1M input tokens).
/// ModelPricing uses microcents per 1M tokens for precision.
///
/// Conversion: dollars * 1_000_000 (to microcents)
pub fn catalog_cost_to_model_pricing(cost: &CatalogCost) -> ModelPricing {
    ModelPricing {
        input_per_1m_tokens: dollars_to_microcents(cost.input),
        output_per_1m_tokens: dollars_to_microcents(cost.output),
        reasoning_per_1m_tokens: cost.reasoning.map(dollars_to_microcents),
        cached_input_per_1m_tokens: cost.cache_read.map(dollars_to_microcents),
        cache_write_per_1m_tokens: cost.cache_write.map(dollars_to_microcents),
        ..Default::default()
    }
}

/// Convert dollars to microcents (1/1,000,000 of a dollar).
fn dollars_to_microcents(dollars: f64) -> i64 {
    (dollars * 1_000_000.0).round() as i64
}

/// Resolve the catalog provider ID for a Hadrian provider configuration.
///
/// This maps Hadrian's internal provider types to the models.dev provider IDs.
/// For OpenAI-compatible providers, it attempts to detect the provider from the base URL.
pub fn resolve_catalog_provider_id(
    provider_type: &str,
    base_url: Option<&str>,
    explicit_catalog_provider: Option<&str>,
) -> Option<String> {
    // Explicit override takes precedence
    if let Some(explicit) = explicit_catalog_provider {
        return Some(explicit.to_string());
    }

    match provider_type {
        "anthropic" => Some("anthropic".to_string()),
        "bedrock" => Some("amazon-bedrock".to_string()),
        "vertex" => Some("google-vertex".to_string()),
        // Gemini Developer API serves the same Google models; reuse the
        // google-vertex catalog id for pricing/enrichment.
        "gemini" => Some("google-vertex".to_string()),
        "azure_openai" => Some("azure".to_string()),
        "test" => None,
        "openai" => {
            // Detect provider from base URL
            if let Some(url) = base_url {
                detect_provider_from_url(url)
            } else {
                Some("openai".to_string())
            }
        }
        _ => None,
    }
}

/// Detect the catalog provider ID from an API base URL.
fn detect_provider_from_url(url: &str) -> Option<String> {
    let url_lower = url.to_lowercase();

    if url_lower.contains("openai.com") {
        Some("openai".to_string())
    } else if url_lower.contains("openrouter.ai") {
        Some("openrouter".to_string())
    } else if url_lower.contains("groq.com") || url_lower.contains("groq.ai") {
        Some("groq".to_string())
    } else if url_lower.contains("together.xyz") || url_lower.contains("together.ai") {
        Some("together".to_string())
    } else if url_lower.contains("mistral.ai") {
        Some("mistral".to_string())
    } else if url_lower.contains("deepinfra.com") {
        Some("deepinfra".to_string())
    } else if url_lower.contains("fireworks.ai") {
        Some("fireworks-ai".to_string())
    } else if url_lower.contains("perplexity.ai") {
        Some("perplexity".to_string())
    } else if url_lower.contains("deepseek.com") {
        Some("deepseek".to_string())
    } else if url_lower.contains("cohere.ai") || url_lower.contains("cohere.com") {
        Some("cohere".to_string())
    } else if url_lower.contains("anyscale.com") {
        Some("anyscale".to_string())
    } else if url_lower.contains("replicate.com") {
        Some("replicate".to_string())
    } else if url_lower.contains("cerebras.ai") {
        Some("cerebras".to_string())
    } else {
        // Default to openai for unknown OpenAI-compatible endpoints
        Some("openai".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_from_json() {
        let json = r#"{
            "anthropic": {
                "id": "anthropic",
                "name": "Anthropic",
                "models": {
                    "claude-opus-4-5": {
                        "id": "claude-opus-4-5",
                        "name": "Claude Opus 4.5",
                        "family": "claude-opus",
                        "attachment": true,
                        "reasoning": true,
                        "tool_call": true,
                        "temperature": true,
                        "cost": {
                            "input": 5.0,
                            "output": 25.0,
                            "cache_read": 0.5
                        },
                        "limit": {
                            "context": 200000,
                            "output": 64000
                        },
                        "modalities": {
                            "input": ["text", "image"],
                            "output": ["text"]
                        }
                    }
                }
            }
        }"#;

        let registry = ModelCatalogRegistry::new();
        registry.load_from_json(json).unwrap();

        assert_eq!(registry.model_count(), 1);

        let enrichment = registry.lookup("anthropic", "claude-opus-4-5").unwrap();
        assert!(enrichment.capabilities.vision);
        assert!(enrichment.capabilities.reasoning);
        assert!(enrichment.capabilities.tool_call);
        assert_eq!(enrichment.limits.context_length, Some(200000));
        assert_eq!(enrichment.limits.max_output_tokens, Some(64000));
        assert_eq!(enrichment.family, Some("claude-opus".to_string()));
        assert_eq!(enrichment.modalities.input, vec!["text", "image"]);
    }

    #[test]
    fn test_catalog_cost_to_model_pricing() {
        let cost = CatalogCost {
            input: 5.0,   // $5 per 1M tokens
            output: 25.0, // $25 per 1M tokens
            reasoning: Some(15.0),
            cache_read: Some(0.5),
            cache_write: Some(6.25),
            input_audio: None,
            output_audio: None,
        };

        let pricing = catalog_cost_to_model_pricing(&cost);

        // $5 = 5_000_000 microcents
        assert_eq!(pricing.input_per_1m_tokens, 5_000_000);
        // $25 = 25_000_000 microcents
        assert_eq!(pricing.output_per_1m_tokens, 25_000_000);
        // $15 = 15_000_000 microcents
        assert_eq!(pricing.reasoning_per_1m_tokens, Some(15_000_000));
        // $0.5 = 500_000 microcents
        assert_eq!(pricing.cached_input_per_1m_tokens, Some(500_000));
        // $6.25 = 6_250_000 microcents
        assert_eq!(pricing.cache_write_per_1m_tokens, Some(6_250_000));
    }

    #[test]
    fn test_resolve_catalog_provider_id() {
        // Explicit override
        assert_eq!(
            resolve_catalog_provider_id("openai", None, Some("custom-provider")),
            Some("custom-provider".to_string())
        );

        // Native providers
        assert_eq!(
            resolve_catalog_provider_id("anthropic", None, None),
            Some("anthropic".to_string())
        );
        assert_eq!(
            resolve_catalog_provider_id("bedrock", None, None),
            Some("amazon-bedrock".to_string())
        );
        assert_eq!(
            resolve_catalog_provider_id("vertex", None, None),
            Some("google-vertex".to_string())
        );
        // Gemini reuses the google-vertex catalog id (shared model pricing).
        assert_eq!(
            resolve_catalog_provider_id("gemini", None, None),
            Some("google-vertex".to_string())
        );
        // ...but a per-provider override still wins.
        assert_eq!(
            resolve_catalog_provider_id("gemini", None, Some("google-ai")),
            Some("google-ai".to_string())
        );
        assert_eq!(
            resolve_catalog_provider_id("azure_openai", None, None),
            Some("azure".to_string())
        );

        // Test provider
        assert_eq!(resolve_catalog_provider_id("test", None, None), None);

        // OpenAI with URL detection
        assert_eq!(
            resolve_catalog_provider_id("openai", Some("https://api.openai.com/v1"), None),
            Some("openai".to_string())
        );
        assert_eq!(
            resolve_catalog_provider_id("openai", Some("https://openrouter.ai/api/v1"), None),
            Some("openrouter".to_string())
        );
        assert_eq!(
            resolve_catalog_provider_id("openai", Some("https://api.groq.com/openai/v1"), None),
            Some("groq".to_string())
        );
    }

    #[test]
    fn test_detect_provider_from_url() {
        assert_eq!(
            detect_provider_from_url("https://api.openai.com/v1"),
            Some("openai".to_string())
        );
        assert_eq!(
            detect_provider_from_url("https://openrouter.ai/api/v1"),
            Some("openrouter".to_string())
        );
        assert_eq!(
            detect_provider_from_url("https://api.groq.com/openai/v1"),
            Some("groq".to_string())
        );
        assert_eq!(
            detect_provider_from_url("https://api.together.xyz/v1"),
            Some("together".to_string())
        );
        assert_eq!(
            detect_provider_from_url("https://api.mistral.ai/v1"),
            Some("mistral".to_string())
        );
        assert_eq!(
            detect_provider_from_url("https://api.deepinfra.com/v1/openai"),
            Some("deepinfra".to_string())
        );
        assert_eq!(
            detect_provider_from_url("https://api.fireworks.ai/inference/v1"),
            Some("fireworks-ai".to_string())
        );
        assert_eq!(
            detect_provider_from_url("https://api.deepseek.com"),
            Some("deepseek".to_string())
        );
        assert_eq!(
            detect_provider_from_url("https://api.cerebras.ai/v1"),
            Some("cerebras".to_string())
        );

        // Unknown URL defaults to openai
        assert_eq!(
            detect_provider_from_url("https://my-custom-llm.example.com/v1"),
            Some("openai".to_string())
        );
    }

    #[test]
    fn test_lookup_missing() {
        let registry = ModelCatalogRegistry::new();
        assert!(registry.lookup("nonexistent", "model").is_none());
    }

    #[test]
    fn test_get_pricing() {
        let json = r#"{
            "openai": {
                "id": "openai",
                "name": "OpenAI",
                "models": {
                    "gpt-4o": {
                        "id": "gpt-4o",
                        "name": "GPT-4o",
                        "cost": {
                            "input": 2.5,
                            "output": 10.0
                        }
                    }
                }
            }
        }"#;

        let registry = ModelCatalogRegistry::new();
        registry.load_from_json(json).unwrap();

        let pricing = registry.get_pricing("openai", "gpt-4o").unwrap();
        assert_eq!(pricing.input_per_1m_tokens, 2_500_000);
        assert_eq!(pricing.output_per_1m_tokens, 10_000_000);
    }
}
